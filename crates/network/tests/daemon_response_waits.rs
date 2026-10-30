#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::json;
use support::*;

#[tokio::test]
async fn immediate_read_survives_eight_positive_waits_without_cancelling_them() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("reader", &state.0).await;
    for id in 10..18 {
        client
            .send(json!({"id":id,"op":"inbox","auth":auth,"wait_secs":600}))
            .await;
    }
    let barrier = client
        .request(json!({"id":20,"op":"unread","auth":auth}))
        .await;
    assert_eq!(barrier["result"]["value"]["count"], 0);
    let immediate = client
        .request(json!({"id":21,"op":"inbox","auth":auth}))
        .await;
    assert_eq!(immediate["result"]["status"], "ok");
    assert!(
        immediate["result"]["value"]["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // None of the positive replies should precede this ordinary command: the
    // immediate guard did not alter their generation or cancel their waits.
    let barrier = client
        .request(json!({"id":22,"op":"unread","auth":auth}))
        .await;
    assert_eq!(barrier["result"]["value"]["count"], 0);
    client
        .send(json!({"id":23,"op":"stop_waiting","auth":auth}))
        .await;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..9 {
        let reply = client.receive().await;
        assert_eq!(reply["result"]["status"], "ok");
        seen.insert(reply["id"].as_u64().unwrap());
    }
    assert_eq!(seen, (10..18).chain([23]).collect());
    client
        .request(json!({"id":24,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn disconnected_immediate_read_and_positive_control_remain_unread_after_sqlite_release() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    for seconds in [600, 0] {
        let state = State::new();
        let mut child = state.start().await;
        let mut client = state.connect().await;
        let auth = client.register("reader", &state.0).await;
        client
            .request(
                json!({"id":2,"op":"send","auth":auth,"message_id":"retained",
            "to":{"name":"reader"},"body":"keep after EOF"}),
            )
            .await;
        let mut observer = state.connect().await;
        observer.request(json!({"id":3,"op":"who"})).await;
        let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
        sql.execute_batch("BEGIN IMMEDIATE").unwrap();
        client
            .send(json!({"id":4,"op":"inbox","auth":auth,"wait_secs":seconds}))
            .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        observer.send(json!({"id":5,"op":"who"})).await;
        let blocked = tokio::time::timeout(Duration::from_millis(100), observer.receive())
            .await
            .is_err();
        client.0.get_mut().shutdown().await.unwrap();
        drop(client);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let before: Option<i64> = sql
            .query_row("SELECT read_ms FROM mail WHERE id='retained'", [], |row| {
                row.get(0)
            })
            .unwrap();
        sql.execute_batch("ROLLBACK").unwrap();
        if blocked {
            assert_eq!(observer.receive().await["id"], 5);
        }
        observer
            .request(json!({"id":6,"op":"attach","auth":auth}))
            .await;
        observer
            .request(json!({"id":7,"op":"shutdown","auth":auth}))
            .await;
        exited(&mut child).await;
        drop(observer);
        let after: Option<i64> = sql
            .query_row("SELECT read_ms FROM mail WHERE id='retained'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let reopened = foundry_network::mail_store::MailStore::open(&state.0).unwrap();
        let message = reopened.get("retained").unwrap().message;
        assert!(blocked, "preparation never reached the held SQLite gate");
        assert_eq!(before, None);
        assert_eq!(
            after, None,
            "disconnected inbox consumed after SQLite resumed"
        );
        assert_eq!(message.body, b"keep after EOF");
        assert_eq!(reopened.unread(&message.recipient).unwrap(), 1);
    }
}
