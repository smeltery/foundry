use super::{
    config::Root,
    database::{claimed, journal_stores, transport, uncontended},
    flow::*,
};
use serde_json::json;
use std::time::Duration;

// Far shorter than any network wait the frozen peer causes (the 5s receipt
// deadline, then 10s dials), yet far longer than any local store commit.
const PATIENCE: Duration = Duration::from_secs(1);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_peer_io_holds_no_store_transaction_or_local_control() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let outbox = a.config.state.join("mail.sqlite3");
    // The frozen child keeps its sockets open but never reads or acknowledges.
    b.signal(libc::SIGSTOP);
    send(&mut ca, &aa, &b.config, "into-frozen", "awaits a receipt").await;
    let attempt = claimed(&outbox, "into-frozen").await;
    for store in journal_stores(&a.config.state) {
        uncontended(&store, PATIENCE);
    }
    // Reads and mutations on the sender proceed while its receipt wait is open.
    // Rotation and invitation commit to the WAL team store through the same
    // gate every team-store transaction uses, so they prove that store free too.
    ca.status(&aa).await;
    ca.ok(json!({"op":"who","filter":b.config.destination()}))
        .await;
    ca.ok(json!({"op":"inbox","auth":aa,"peek":true})).await;
    send(&mut ca, &aa, &b.config, "behind-frozen", "queued meanwhile").await;
    ca.control(&aa, json!({"kind":"rotate"})).await;
    invite(&mut ca, &aa, false).await;
    for store in journal_stores(&a.config.state) {
        uncontended(&store, PATIENCE);
    }
    assert_eq!(
        transport(&outbox, "into-frozen").unwrap().1,
        Some(attempt),
        "the original network wait ended before local work finished"
    );
    b.signal(libc::SIGCONT);
    let expected = [
        ("behind-frozen", "queued meanwhile"),
        ("into-frozen", "awaits a receipt"),
    ];
    // Redelivery after a retired attempt may reorder the two texts.
    let mut arrived = tokio::time::timeout(super::DEADLINE, async {
        loop {
            let value = cb.ok(json!({"op":"inbox","auth":ab,"peek":true})).await;
            if value["messages"].as_array().unwrap().len() >= expected.len() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("durable inbox arrival deadline");
    sort(&mut arrived);
    check_mail(&arrived, &expected, "pending");
    let mut read = cb.ok(json!({"op":"inbox","auth":ab})).await;
    sort(&mut read);
    check_mail(&read, &expected, "consumed");
    consume(&mut cb, &ab, &[]).await;
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
}
fn sort(inbox: &mut serde_json::Value) {
    inbox["messages"]
        .as_array_mut()
        .unwrap()
        .sort_by(|x, y| x["message_id"].as_str().cmp(&y["message_id"].as_str()));
}
