use super::{child::Node, client::Client, config::Root, flow::*};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_receiver_and_crashed_sender_replay_queued_text_exactly_once_after_restart() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, invitation) = pair(&root).await;
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    wait_remote(&mut ca, &cfg_b, false).await;
    send(
        &mut ca,
        &aa,
        &cfg_b,
        "offline-one",
        "queued while recipient was offline",
    )
    .await;
    let cfg_a = a.kill().await;
    let a = Node::start(cfg_a).await;
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let mut cfg_b = cfg_b;
    let address: iroh::EndpointAddr =
        serde_json::from_value(ca.status(&aa).await["endpoint"].clone()).unwrap();
    seed(&mut cfg_b, &address);
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    wait_remote(&mut ca, &b.config, true).await;
    peek(
        &mut cb,
        &ab,
        &[("offline-one", "queued while recipient was offline")],
    )
    .await;
    consume(
        &mut cb,
        &ab,
        &[("offline-one", "queued while recipient was offline")],
    )
    .await;
    // A second receiver restart cannot materialize another copy of the old queue item.
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    wait_remote(&mut cb, &a.config, true).await;
    consume(&mut cb, &ab, &[]).await;
    b.stop(&mut cb, &ab, &[&invitation.ticket]).await;
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_invitation_uses_real_listener_consumes_once_and_active_startup_does_not_redeem_again()
 {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, true).await;
    let code = invitation.code.as_ref().expect("short invitation code");
    let address = invitation
        .code_endpoint
        .as_ref()
        .expect("derived code listener address");
    let mut config = root.config("b", "blair");
    seed(&mut config, address);
    seed(&mut config, &invitation.endpoint);
    config.value["network"]["startup"] = json!({"mode":"join_code","code":code});
    config.save();
    let b = Node::start(config).await;
    let (mut cb, ab) = b.client().await;
    let team = cb.status(&ab).await["team"].clone();
    assert_eq!(team, ca.status(&aa).await["team"]);
    wait_remote(&mut ca, &b.config, true).await;
    wait_remote(&mut cb, &a.config, true).await;
    send(
        &mut ca,
        &aa,
        &b.config,
        "short-code-text",
        "real short code delivery",
    )
    .await;
    peek(
        &mut cb,
        &ab,
        &[("short-code-text", "real short code delivery")],
    )
    .await;
    consume(
        &mut cb,
        &ab,
        &[("short-code-text", "real short code delivery")],
    )
    .await;
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[code]).await;
    // Same consumed startup code, same protected store: resume membership only.
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    assert_eq!(cb.status(&ab).await["team"], team);
    let c = Node::start(root.config("c", "casey")).await;
    let (mut cc, ac) = c.client().await;
    let reply = cc
        .request(json!({"op":"network","auth":ac,
        "command":{"kind":"join_code","code":code,"address":address}}))
        .await;
    assert_eq!(
        reply["result"]["status"], "error",
        "consumed invitation admitted another process"
    );
    assert!(cc.status(&ac).await["team"].is_null());
    c.stop(&mut cc, &ac, &[code]).await;
    b.stop(&mut cb, &ab, &[code]).await;
    a.stop(&mut ca, &aa, &[code, &invitation.ticket]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_peer_cannot_resume_admitted_delivery_or_silently_reform() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let id: iroh::EndpointAddr =
        serde_json::from_value(cb.status(&ab).await["endpoint"].clone()).unwrap();
    ca.control(&aa, json!({"kind":"remove","endpoints":[id.id]}))
        .await;
    let reply = ca
        .request(json!({"op":"send","auth":aa,"message_id":"after-removal",
        "to":b.config.destination(),"body":"must not be admitted"}))
        .await;
    assert_eq!(reply["result"]["status"], "error");
    let value = cb.ok(json!({"op":"inbox","auth":ab})).await;
    check_mail(&value, &[], "consumed");
    let original_team = ca.status(&aa).await["team"].clone();
    let cfg_b = b.kill().await;
    // Removal may already be durable locally, or the peer may retain its older
    // signed checkpoint. Neither case authorizes a reset or a new admission.
    if let Some(b) = Node::start_or_rejected(cfg_b).await {
        let mut cb = Client::connect(&b.config).await;
        cb.attach(&ab).await;
        assert_eq!(cb.status(&ab).await["team"], original_team);
        cb.request(json!({"op":"network","auth":ab,
            "command":{"kind":"connect","address":ca.status(&aa).await["endpoint"]}}))
            .await;
        let attempted = cb
            .request(
                json!({"op":"send","auth":ab,"message_id":"revoked-peer-attempt",
            "to":a.config.destination(),"body":"must not enter current member inbox"}),
            )
            .await;
        if attempted["result"]["status"] == "ok" {
            assert_eq!(attempted["result"]["value"]["kind"], "transport_queued");
        }
        b.stop(&mut cb, &ab, &[]).await;
        let inbox = ca.ok(json!({"op":"inbox","auth":aa})).await;
        check_mail(&inbox, &[], "consumed");
    }
    let reply = ca
        .request(json!({"op":"send","auth":aa,"message_id":"still-removed",
        "to":{"person":"blair","name":"terminal-b"},"body":"must stay rejected"}))
        .await;
    assert_eq!(reply["result"]["status"], "error");
    a.stop(&mut ca, &aa, &[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unconsumed_code_intent_recovers_its_actual_listener_after_creator_restart() {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, true).await;
    let code = invitation.code.as_ref().unwrap();
    let intent = invitation.intent.as_ref().unwrap();
    let config = a.config.clone();
    a.stop(&mut ca, &aa, &[code]).await;
    let a = Node::start(config).await;
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let recovered = publication(&mut ca, &aa, intent).await;
    assert!(recovered.id == invitation.code_endpoint.as_ref().unwrap().id);
    let mut config = root.config("b", "blair");
    seed(&mut config, &recovered);
    let endpoint: iroh::EndpointAddr =
        serde_json::from_value(ca.status(&aa).await["endpoint"].clone()).unwrap();
    seed(&mut config, &endpoint);
    let b = Node::start(config).await;
    let (mut cb, ab) = b.client().await;
    cb.control(
        &ab,
        json!({"kind":"join_code","code":code,"address":recovered}),
    )
    .await;
    wait_remote(&mut ca, &b.config, true).await;
    send(
        &mut ca,
        &aa,
        &b.config,
        "recovered-listener-text",
        "real recovered code delivery",
    )
    .await;
    peek(
        &mut cb,
        &ab,
        &[("recovered-listener-text", "real recovered code delivery")],
    )
    .await;
    consume(
        &mut cb,
        &ab,
        &[("recovered-listener-text", "real recovered code delivery")],
    )
    .await;
    tokio::time::timeout(super::DEADLINE, async {
        loop {
            let value = ca.control(&aa, json!({"kind":"code_publications"})).await;
            if value["publications"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["intent"] != *intent)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("consumed recovered listener retirement");
    b.stop(&mut cb, &ab, &[code]).await;
    a.stop(&mut ca, &aa, &[code]).await;
}
