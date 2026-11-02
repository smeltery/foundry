use super::{
    DEADLINE,
    child::Node,
    client::Client,
    config::Root,
    database::{WriteLock, received, transport},
    flow::*,
};
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_receiver_survives_lost_sender_receipt_settlement_and_replay_deduplicates() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let path_a = a.config.state.join("mail.sqlite3");
    let path_b = b.config.state.join("mail.sqlite3");
    let receiving = WriteLock::new(&path_b);
    send(
        &mut ca,
        &aa,
        &b.config,
        "lost-settlement",
        "immutable replay fixture",
    )
    .await;
    let original = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((payload, Some(_))) = transport(&path_a, "lost-settlement") {
                break payload;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual transport claim before receiver commit");
    let unsettled = WriteLock::new(&path_a);
    assert!(
        receiving.elapsed() < Duration::from_secs(3),
        "receiver SQLite busy timeout would invalidate staging"
    );
    drop(receiving);
    peek(
        &mut cb,
        &ab,
        &[("lost-settlement", "immutable replay fixture")],
    )
    .await;
    let accepted = received(&path_b, "lost-settlement");
    assert!(
        transport(&path_a, "lost-settlement").is_some(),
        "receipt was unexpectedly durably settled"
    );
    let cfg_a = a.kill().await;
    assert!(transport(&path_a, "lost-settlement").unwrap().0 == original);
    drop(unsettled);
    let a = Node::start(cfg_a).await;
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let endpoint = ca.status(&aa).await["endpoint"].clone();
    cb.control(&ab, json!({"kind":"connect","address":endpoint}))
        .await;
    tokio::time::timeout(DEADLINE, async {
        while transport(&path_a, "lost-settlement").is_some() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("matching replay receipt settlement");
    peek(
        &mut cb,
        &ab,
        &[("lost-settlement", "immutable replay fixture")],
    )
    .await;
    assert!(
        received(&path_b, "lost-settlement") == accepted,
        "replay rewrote immutable inbound bytes"
    );
    consume(
        &mut cb,
        &ab,
        &[("lost-settlement", "immutable replay fixture")],
    )
    .await;
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
}
