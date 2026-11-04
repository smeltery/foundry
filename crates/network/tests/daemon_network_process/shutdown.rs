use super::{
    DEADLINE,
    child::Node,
    client::Client,
    config::Root,
    database::{claimed, transport},
    flow::*,
};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_shutdown_keeps_unacknowledged_text_for_exactly_one_redelivery() {
    let root = Root::new();
    let (a, mut ca, aa, b, _, ab, invitation) = pair(&root).await;
    let outbox = a.config.state.join("mail.sqlite3");
    let text = [("unacknowledged", "in flight during shutdown")];
    // Frozen before the send, the recipient can neither store nor ACK it.
    b.signal(libc::SIGSTOP);
    send(&mut ca, &aa, &b.config, text[0].0, text[0].1).await;
    claimed(&outbox, text[0].0).await;
    let original = transport(&outbox, text[0].0).unwrap().0;
    let cfg_a = a.config.clone();
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
    assert!(
        transport(&outbox, text[0].0).is_some_and(|(payload, _)| payload == original),
        "clean shutdown settled or rewrote an unacknowledged text"
    );
    let mut cfg_b = b.kill().await;
    let a = Node::start(cfg_a).await;
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let address = serde_json::from_value(ca.status(&aa).await["endpoint"].clone()).unwrap();
    seed(&mut cfg_b, &address);
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    peek(&mut cb, &ab, &text).await;
    tokio::time::timeout(DEADLINE, async {
        while transport(&outbox, text[0].0).is_some() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("redelivery receipt settlement");
    consume(&mut cb, &ab, &text).await;
    // A reconnected recipient cannot materialize a second copy.
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    wait_remote(&mut cb, &a.config, true).await;
    wait_remote(&mut ca, &b.config, true).await;
    consume(&mut cb, &ab, &[]).await;
    assert!(transport(&outbox, text[0].0).is_none());
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
}
