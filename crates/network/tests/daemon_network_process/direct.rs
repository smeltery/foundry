use super::{client::Client, config::Root, flow::*};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_children_form_join_exchange_bidirectional_durable_mail_and_drain() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, invitation) = pair(&root).await;
    send(&mut ca, &aa, &b.config, "a-to-b", "original direct payload").await;
    peek(&mut cb, &ab, &[("a-to-b", "original direct payload")]).await;
    let unread = cb.ok(json!({"op":"unread","auth":ab})).await;
    assert_eq!(
        unread["count"], 1,
        "transport receipt must not consume client mail"
    );
    send(&mut cb, &ab, &a.config, "b-to-a", "original reply payload").await;
    peek(&mut ca, &aa, &[("b-to-a", "original reply payload")]).await;
    consume(&mut ca, &aa, &[("b-to-a", "original reply payload")]).await;
    consume(&mut cb, &ab, &[("a-to-b", "original direct payload")]).await;
    let cfg_a = a.config.clone();
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[&invitation.ticket]).await;
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
    // Shutdown returns only after owned state can be reopened by real children.
    let a = super::child::Node::start(cfg_a).await;
    let b = super::child::Node::start(cfg_b).await;
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    assert_eq!(ca.status(&aa).await["team"], cb.status(&ab).await["team"]);
    consume(&mut ca, &aa, &[]).await;
    consume(&mut cb, &ab, &[]).await;
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_controls_require_current_connection_credentials_and_strict_fields() {
    let root = Root::new();
    let node = super::child::Node::start(root.config("a", "alex")).await;
    let (mut client, auth) = node.client().await;
    let forged = json!({"conversation":auth["conversation"],"credential":vec![0u8;32]});
    let reply = client
        .request(json!({"op":"network","auth":forged,"command":{"kind":"form","name":"forbidden"}}))
        .await;
    assert_eq!(reply["result"]["status"], "error");
    assert!(client.status(&auth).await["team"].is_null());
    for command in [
        json!({"kind":"status","extra":true}),
        json!({"kind":"form","name":"team","unknown":true}),
    ] {
        let reply = client
            .request(json!({"op":"network","auth":auth,"command":command}))
            .await;
        assert_eq!(reply["result"]["status"], "error");
    }
    let mut unattached = Client::connect(&node.config).await;
    let reply = unattached
        .request(json!({"op":"network","auth":auth,"command":{"kind":"status"}}))
        .await;
    assert_eq!(reply["result"]["status"], "error");
    node.stop(&mut client, &auth, &[]).await;
}
