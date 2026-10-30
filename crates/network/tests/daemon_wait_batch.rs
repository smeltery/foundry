#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::json;
use support::*;
#[tokio::test]
async fn real_process_default_batch_consumes_all_once_and_no_other_conversation() {
    let state = State::new();
    let mut child = state.start().await;
    let mut sender = state.connect().await;
    let mut receiver = state.connect().await;
    let a = sender.register("a", &state.0).await;
    let b = receiver.register("b", &state.0).await;
    for (id, to, body) in [
        ("one", "b", "first body"),
        ("two", "b", "second body"),
        ("other", "a", "other body"),
    ] {
        assert_eq!(sender.request(json!({"id":2,"op":"send","auth":a,"message_id":id,"to":{"name":to},"body":body})).await["result"]["status"],"ok");
    }
    let reply = receiver
        .request(json!({"id":3,"op":"inbox","auth":b}))
        .await;
    let value = &reply["result"]["value"];
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["message_id"], "one");
    assert_eq!(messages[1]["message_id"], "two");
    assert!(
        messages
            .iter()
            .all(|m| m["state"] == "consumed" && m["attempt_id"].is_null())
    );
    assert!(value["text"].as_str().unwrap().contains("first body"));
    assert!(value["text"].as_str().unwrap().contains("second body"));
    let empty = receiver
        .request(json!({"id":4,"op":"inbox","auth":b}))
        .await;
    assert!(
        empty["result"]["value"]["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(empty["result"]["value"]["text"], "");
    assert_eq!(
        sender.request(json!({"id":5,"op":"unread","auth":a})).await["result"]["value"]["count"],
        1
    );
    let forged = json!({"conversation":a["conversation"],"credential":b["credential"]});
    assert_eq!(
        receiver
            .request(json!({"id":6,"op":"inbox","auth":forged}))
            .await["result"]["status"],
        "error"
    );
    receiver
        .request(json!({"id":7,"op":"shutdown","auth":b}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn real_process_batch_peek_keeps_all_and_acknowledges_subset() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("reader", &state.0).await;
    for id in ["one", "two"] {
        client.request(json!({"id":2,"op":"send","auth":auth,"message_id":id,"to":{"name":"reader"},"body":id})).await;
    }
    let reply = client
        .request(json!({"id":3,"op":"inbox","auth":auth,"peek":true}))
        .await;
    let messages = reply["result"]["value"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let first = messages[0]["attempt_id"].clone();
    let second = messages[1]["attempt_id"].clone();
    assert_eq!(
        client
            .request(json!({"id":4,"op":"unread","auth":auth}))
            .await["result"]["value"]["count"],
        2
    );
    client
        .request(json!({"id":5,"op":"delivered","auth":auth,"attempt_id":first}))
        .await;
    let reply = client
        .request(json!({"id":6,"op":"inbox","auth":auth,"peek":true}))
        .await;
    let remaining = reply["result"]["value"]["messages"].as_array().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["attempt_id"], second);
    assert_eq!(remaining[0]["message_id"], "two");
    client
        .request(json!({"id":7,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn real_process_malformed_later_payload_preserves_earlier_unread() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("reader", &state.0).await;
    for id in ["one", "bad"] {
        client.request(json!({"id":2,"op":"send","auth":auth,"message_id":id,"to":{"name":"reader"},"body":id})).await;
    }
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    sql.execute("UPDATE mail SET encoding=1 WHERE id='bad'", [])
        .unwrap();
    let failed = client
        .request(json!({"id":3,"op":"inbox","auth":auth}))
        .await;
    assert_eq!(failed["result"]["value"], "invalid_payload");
    assert_eq!(
        client
            .request(json!({"id":4,"op":"unread","auth":auth}))
            .await["result"]["value"]["count"],
        2
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM mail WHERE read_ms IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(sql);
    client
        .request(json!({"id":5,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
#[tokio::test]
async fn real_process_large_batch_peeks_then_consumes_the_complete_snapshot() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("reader", &state.0).await;
    for n in 0..3 {
        client.request(json!({"id":2,"op":"send","auth":auth,"message_id":format!("m{n}"),"to":{"name":"reader"},"body":"a".repeat(150*1024)})).await;
    }
    for peek in [true, false] {
        let reply = client
            .request(json!({"id":3,"op":"inbox","auth":auth,"peek":peek}))
            .await;
        assert_eq!(reply["result"]["status"], "ok");
        assert!(serde_json::to_vec(&reply).unwrap().len() > 1024 * 1024);
        let messages = reply["result"]["value"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        for (index, message) in messages.iter().enumerate() {
            assert_eq!(message["message_id"], format!("m{index}"));
            assert_eq!(message["state"], if peek { "pending" } else { "consumed" });
            let body = data_encoding::BASE64
                .decode(message["body_base64"].as_str().unwrap().as_bytes())
                .unwrap();
            assert_eq!(body, vec![b'a'; 150 * 1024]);
        }
        assert_eq!(
            client
                .request(json!({"id":4,"op":"unread","auth":auth}))
                .await["result"]["value"]["count"],
            if peek { 3 } else { 0 }
        );
    }
    assert_eq!(
        client
            .request(json!({"id":4,"op":"unread","auth":auth}))
            .await["result"]["value"]["count"],
        0
    );
    client
        .request(json!({"id":5,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
