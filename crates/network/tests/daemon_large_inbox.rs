#![cfg(unix)]
#[path = "daemon_large_inbox/mod.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "daemon_process_support/mod.rs"]
mod support;

use fixture::*;
use serde_json::json;

const MANY: usize = 137;
const SMALL_BODY: &str = "small valid payload";

#[tokio::test]
async fn process_consumes_more_than_128_in_order_once_and_preserves_other_mailbox() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = Client::connect(&state).await;
    let sender = client.register("sender", &state.0).await;
    let reader = client.register("reader", &state.0).await;
    client.enqueue(&sender, MANY, SMALL_BODY).await;
    client
        .ok(
            json!({"op":"send","auth":sender,"message_id":"separate-mailbox",
        "to":{"name":"sender"},"body":"keep this pending"}),
        )
        .await;
    let forged = json!({"conversation":reader["conversation"],"credential":sender["credential"]});
    let refused = client.request(json!({"op":"inbox","auth":forged})).await;
    assert_eq!(refused["result"]["status"], "error");
    client.unread(&reader, MANY).await;
    let batch = client.ok(json!({"op":"inbox","auth":reader})).await;
    check_messages(&batch, &(0..MANY).collect::<Vec<_>>(), SMALL_BODY, false);
    client.unread(&reader, 0).await;
    let empty = client.ok(json!({"op":"inbox","auth":reader})).await;
    check_messages(&empty, &[], SMALL_BODY, false);
    client.unread(&sender, 1).await;
    let other = client
        .ok(json!({"op":"inbox","auth":sender,"peek":true}))
        .await;
    let other = other["messages"].as_array().unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0]["message_id"], "separate-mailbox");
    assert_eq!(other[0]["state"], "pending");
    client.ok(json!({"op":"shutdown","auth":reader})).await;
    exited(&mut child).await;
}

#[tokio::test]
async fn process_peeks_more_than_128_then_subset_ack_and_restart_preserve_remainder() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = Client::connect(&state).await;
    let auth = client.register("reader", &state.0).await;
    client.enqueue(&auth, MANY, SMALL_BODY).await;
    let all: Vec<_> = (0..MANY).collect();
    let peek = client
        .ok(json!({"op":"inbox","auth":auth,"peek":true}))
        .await;
    let tokens = check_messages(&peek, &all, SMALL_BODY, true);
    client.unread(&auth, MANY).await;
    let again = client
        .ok(json!({"op":"inbox","auth":auth,"peek":true}))
        .await;
    assert_eq!(check_messages(&again, &all, SMALL_BODY, true), tokens);
    for index in (0..MANY).step_by(3) {
        client
            .ok(json!({"op":"delivered","auth":auth,"attempt_id":tokens[index]}))
            .await;
    }
    let remaining: Vec<_> = all.into_iter().filter(|index| index % 3 != 0).collect();
    client.unread(&auth, remaining.len()).await;
    client.ok(json!({"op":"shutdown","auth":auth})).await;
    exited(&mut child).await;
    drop(client);
    let mut child = state.start().await;
    let mut client = Client::connect(&state).await;
    client.ok(json!({"op":"attach","auth":auth})).await;
    client.unread(&auth, remaining.len()).await;
    let peek = client
        .ok(json!({"op":"inbox","auth":auth,"peek":true}))
        .await;
    check_messages(&peek, &remaining, SMALL_BODY, true);
    let consumed = client.ok(json!({"op":"inbox","auth":auth})).await;
    check_messages(&consumed, &remaining, SMALL_BODY, false);
    client.unread(&auth, 0).await;
    let empty = client.ok(json!({"op":"inbox","auth":auth})).await;
    check_messages(&empty, &[], SMALL_BODY, false);
    client.ok(json!({"op":"shutdown","auth":auth})).await;
    exited(&mut child).await;
}

#[tokio::test]
async fn process_returns_one_complete_large_reply_but_rejects_large_request() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = Client::connect(&state).await;
    let auth = client.register("reader", &state.0).await;
    // Each individual text is valid and below 64 KiB; only the aggregate is large.
    let body = "q".repeat(60 * 1024);
    client.enqueue(&auth, 20, &body).await;
    reject_oversized_request(&state, &auth).await;
    client.unread(&auth, 20).await;
    let batch = client.ok(json!({"op":"inbox","auth":auth})).await;
    assert!(client.last_record_bytes > 1024 * 1024);
    check_messages(&batch, &(0..20).collect::<Vec<_>>(), &body, false);
    // A correlated next reply also detects spurious extra JSON records.
    client.unread(&auth, 0).await;
    let empty = client.ok(json!({"op":"inbox","auth":auth})).await;
    check_messages(&empty, &[], &body, false);
    client.ok(json!({"op":"shutdown","auth":auth})).await;
    exited(&mut child).await;
}
