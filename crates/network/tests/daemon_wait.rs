#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::json;
use std::time::{Duration, Instant};
use support::*;
async fn barrier(client: &mut Client) {
    // Per-connection ordering proves the wait guard registered, without a sleep.
    client.send(json!({"id":99,"op":"who"})).await;
    assert_eq!(client.receive().await["id"], 99);
}
#[tokio::test]
async fn actual_wait_arrival_consumes_before_reply_and_restart_retains_read_state() {
    let state = State::new();
    let mut child = state.start().await;
    let mut sender = state.connect().await;
    let mut receiver = state.connect().await;
    let a = sender.register("sender", &state.0).await;
    let b = receiver.register("receiver", &state.0).await;
    receiver
        .send(json!({"id":2,"op":"inbox","auth":b,"wait_secs":600}))
        .await;
    barrier(&mut receiver).await;
    let sent=sender.request(json!({"id":3,"op":"send","auth":a,"message_id":"arrival","to":{"name":"receiver"},"body":"hello"})).await;
    assert_eq!(sent["result"]["status"], "ok");
    let reply = receiver.receive().await;
    assert_eq!(reply["id"], 2);
    let mail = &reply["result"]["value"]["messages"][0];
    assert_eq!(mail["message_id"], "arrival");
    assert_eq!(mail["state"], "consumed");
    assert!(mail["attempt_id"].is_null());
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    let (status, read): (i64, Option<i64>) = sql
        .query_row(
            "SELECT state,read_ms FROM mail WHERE id='arrival'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, 0);
    assert!(read.is_some());
    drop(sql);
    receiver
        .request(json!({"id":4,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
    drop(sender);
    drop(receiver);
    let mut child = state.start().await;
    let mut receiver = state.connect().await;
    receiver
        .request(json!({"id":5,"op":"attach","auth":b}))
        .await;
    assert_eq!(
        receiver
            .request(json!({"id":6,"op":"unread","auth":b}))
            .await["result"]["value"]["count"],
        0
    );
    assert!(
        receiver
            .request(json!({"id":7,"op":"inbox","auth":b}))
            .await["result"]["value"]["messages"][0]
            .is_null()
    );
    receiver
        .request(json!({"id":8,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn stop_waiting_on_same_connection_is_scoped_and_oversized_wait_is_bounded() {
    let state = State::new();
    let mut child = state.start().await;
    let mut a = state.connect().await;
    let mut b = state.connect().await;
    let auth_a = a.register("a", &state.0).await;
    let auth_b = b.register("b", &state.0).await;
    a.send(json!({"id":2,"op":"inbox","auth":auth_a,"wait_secs":u64::MAX,"peek":true}))
        .await;
    barrier(&mut a).await;
    b.send(json!({"id":3,"op":"inbox","auth":auth_b,"wait_secs":600,"peek":true}))
        .await;
    barrier(&mut b).await;
    let forged = json!({"conversation":auth_b["conversation"],"credential":auth_a["credential"]});
    assert_eq!(
        a.request(json!({"id":4,"op":"stop_waiting","auth":forged}))
            .await["result"]["value"],
        "unauthorized"
    );
    a.send(json!({"id":5,"op":"stop_waiting","auth":auth_a}))
        .await;
    let one = a.receive().await;
    let two = a.receive().await;
    for reply in [&one, &two] {
        if reply["id"] == 2 {
            assert!(reply["result"]["value"]["messages"][0].is_null());
        } else {
            assert_eq!(reply["id"], 5);
            assert_eq!(reply["result"]["value"]["kind"], "waiting_stopped");
        }
    }
    a.request(json!({"id":6,"op":"send","auth":auth_a,"message_id":"for-b","to":{"name":"b"},"body":"untouched wait"})).await;
    let arrival = b.receive().await;
    assert_eq!(arrival["id"], 3);
    assert_eq!(
        arrival["result"]["value"]["messages"][0]["message_id"],
        "for-b"
    );
    b.request(json!({"id":7,"op":"shutdown","auth":auth_b}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn peek_ack_failure_disconnect_and_restart_preserve_pending_until_confirmed() {
    let state = State::new();
    let mut child = state.start().await;
    let mut sender = state.connect().await;
    let mut receiver = state.connect().await;
    let a = sender.register("sender", &state.0).await;
    let b = receiver.register("receiver", &state.0).await;
    sender.request(json!({"id":2,"op":"send","auth":a,"message_id":"peek","to":{"name":"receiver"},"body":"retained"})).await;
    let first = receiver
        .request(json!({"id":3,"op":"inbox","auth":b,"peek":true}))
        .await;
    let token = first["result"]["value"]["messages"][0]["attempt_id"].clone();
    assert_eq!(first["result"]["value"]["messages"][0]["state"], "pending");
    let again = receiver
        .request(json!({"id":4,"op":"inbox","auth":b,"peek":true}))
        .await;
    assert_eq!(again["result"]["value"]["messages"][0]["attempt_id"], token);
    assert_eq!(
        sender
            .request(json!({"id":5,"op":"delivered","auth":a,"attempt_id":token}))
            .await["result"]["status"],
        "error"
    );
    receiver
        .request(json!({"id":6,"op":"push_failed","auth":b,"attempt_id":token}))
        .await;
    let fresh = receiver
        .request(json!({"id":7,"op":"inbox","auth":b,"peek":true}))
        .await;
    let old = fresh["result"]["value"]["messages"][0]["attempt_id"].clone();
    assert_ne!(old, token);
    child.kill().await.unwrap();
    drop(receiver);
    drop(sender);
    let mut child = state.start().await;
    let mut receiver = state.connect().await;
    receiver
        .request(json!({"id":8,"op":"attach","auth":b}))
        .await;
    assert_eq!(
        receiver
            .request(json!({"id":9,"op":"unread","auth":b}))
            .await["result"]["value"]["count"],
        1
    );
    assert_eq!(
        receiver
            .request(json!({"id":10,"op":"delivered","auth":b,"attempt_id":old}))
            .await["result"]["status"],
        "error"
    );
    let mail = receiver
        .request(json!({"id":11,"op":"inbox","auth":b,"peek":true}))
        .await;
    let token = mail["result"]["value"]["messages"][0]["attempt_id"].clone();
    assert_ne!(token, old);
    receiver
        .request(json!({"id":12,"op":"delivered","auth":b,"attempt_id":token}))
        .await;
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    let status: i64 = sql
        .query_row("SELECT state FROM mail WHERE id='peek'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, 3);
    drop(sql);
    receiver
        .request(json!({"id":13,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn timeout_disconnect_and_shutdown_end_waits_without_stalling_other_requests() {
    let state = State::new();
    let mut child = state.start().await;
    let mut owner = state.connect().await;
    let mut receiver = state.connect().await;
    let a = owner.register("owner", &state.0).await;
    let b = receiver.register("receiver", &state.0).await;
    let started = Instant::now();
    let empty = receiver
        .request(json!({"id":2,"op":"inbox","auth":b,"wait_secs":1}))
        .await;
    assert!(empty["result"]["value"]["messages"][0].is_null());
    assert!(started.elapsed() >= Duration::from_millis(900));
    receiver
        .send(json!({"id":3,"op":"inbox","auth":b,"wait_secs":600}))
        .await;
    barrier(&mut receiver).await;
    drop(receiver);
    owner.request(json!({"id":4,"op":"send","auth":a,"message_id":"offline","to":{"name":"receiver"},"body":"stays unread"})).await;
    let mut receiver = state.connect().await;
    receiver
        .request(json!({"id":5,"op":"attach","auth":b}))
        .await;
    assert_eq!(
        receiver
            .request(json!({"id":6,"op":"unread","auth":b}))
            .await["result"]["value"]["count"],
        1
    );
    owner
        .send(json!({"id":7,"op":"inbox","auth":a,"wait_secs":600}))
        .await;
    barrier(&mut owner).await;
    receiver
        .request(json!({"id":8,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
    drop(owner);
    drop(receiver);
    let mut child = state.start().await;
    let mut receiver = state.connect().await;
    receiver
        .request(json!({"id":9,"op":"attach","auth":b}))
        .await;
    assert_eq!(
        receiver
            .request(json!({"id":10,"op":"unread","auth":b}))
            .await["result"]["value"]["count"],
        1
    );
    receiver
        .request(json!({"id":11,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
}
