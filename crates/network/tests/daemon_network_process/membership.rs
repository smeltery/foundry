use super::{
    child::{Node, fails_to_start},
    client::Client,
    config::Root,
    database::checkpoint,
    flow::*,
};
use serde_json::{Value, json};

async fn refused(client: &mut Client, request: Value, label: &str) {
    let reply = client.request(request).await;
    assert_eq!(
        reply["result"]["status"], "error",
        "{label} passed a revoked membership"
    );
}

fn control(auth: &Value, command: Value) -> Value {
    json!({"op":"network","auth":auth,"command":command})
}
fn connect(address: Value) -> Value {
    json!({"kind":"connect","address":address})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn departed_member_refuses_every_mutation_through_its_durable_gate() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let ticket = invite(&mut ca, &aa, false).await;
    let code = invite(&mut ca, &aa, true).await;
    let founder = ca.status(&aa).await["endpoint"].clone();
    cb.control(&ab, json!({"kind":"depart"})).await;
    let network = |command: Value| json!({"op":"network","auth":ab,"command":command});
    for (label, request) in [
        (
            "form",
            network(json!({"kind":"form","name":"replacement team"})),
        ),
        ("invite", network(json!({"kind":"invite","hours":1}))),
        (
            "short invite",
            network(json!({"kind":"invite","hours":1,"short":true})),
        ),
        (
            "remove",
            network(json!({"kind":"remove","endpoints":[founder["id"]]})),
        ),
        (
            "connect",
            network(json!({"kind":"connect","address":founder})),
        ),
        (
            "join",
            network(json!({"kind":"join","ticket":ticket.ticket,"address":ticket.endpoint})),
        ),
        (
            "join_code",
            network(json!({"kind":"join_code","code":code.code,"address":code.code_endpoint})),
        ),
        (
            "send",
            json!({"op":"send","auth":ab,"message_id":"after-departure",
                "to":a.config.destination(),"body":"must not leave a revoked member"}),
        ),
        ("metadata change", network(json!({"kind":"rotate"}))),
    ] {
        refused(&mut cb, request, label).await;
    }
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    // Read only after the owner exits; an outside WAL reader is not safe live.
    assert_eq!(checkpoint(&cfg_b.state)["state"]["kind"], "removed");
    // The revoked store is never reset or reformed by a later start.
    fails_to_start(&cfg_b).await;
    assert_eq!(checkpoint(&cfg_b.state)["state"]["kind"], "removed");
    // No refused mutation spent the founder's invitations: fresh processes
    // still redeem both the single-use ticket and the short code.
    let mut config = root.config("c", "casey");
    seed(&mut config, &ticket.endpoint);
    let c = Node::start(config).await;
    let (mut cc, ac) = c.client().await;
    join(&mut cc, &ac, &ticket).await;
    let listener = publication(&mut ca, &aa, code.intent.as_ref().unwrap()).await;
    let mut config = root.config("d", "devon");
    seed(&mut config, &listener);
    seed(&mut config, &code.endpoint);
    let d = Node::start(config).await;
    let (mut cd, ad) = d.client().await;
    cd.control(
        &ad,
        json!({"kind":"join_code","code":code.code,"address":listener}),
    )
    .await;
    let team = ca.status(&aa).await["team"].clone();
    assert_eq!(cc.status(&ac).await["team"], team);
    assert_eq!(cd.status(&ad).await["team"], team);
    consume(&mut ca, &aa, &[]).await;
    d.stop(&mut cd, &ad, &[]).await;
    c.stop(&mut cc, &ac, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
}

#[derive(Clone, Copy)]
enum Restart {
    RemoverFirst,
    RemovedFirst,
}

/// Both daemons restart after a durable removal, in the given order; then the
/// remover's gate still refuses the removed peer in both directions.
async fn removal_survives_restart(order: Restart) {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let team = ca.status(&aa).await["team"].clone();
    let removed = cb.status(&ab).await["endpoint"].clone();
    ca.control(&aa, json!({"kind":"remove","endpoints":[removed["id"]]}))
        .await;
    let to_removed = |id: &str| {
        json!({"op":"send","auth":aa,"message_id":id,
        "to":{"person":"blair","name":"terminal-b"},"body":"must stay rejected"})
    };
    refused(&mut ca, to_removed("before-restart"), "send to removed").await;
    let (cfg_a, cfg_b) = (a.config.clone(), b.config.clone());
    b.stop(&mut cb, &ab, &[]).await;
    let (a, b) = match order {
        Restart::RemoverFirst => {
            a.stop(&mut ca, &aa, &[]).await;
            let a = Node::start(cfg_a).await;
            (a, Node::start(cfg_b).await)
        }
        Restart::RemovedFirst => {
            // Its seed still names the running remover, so it dials it first.
            let b = Node::start(cfg_b).await;
            let mut early = Client::connect(&b.config).await;
            early.attach(&ab).await;
            assert_eq!(early.status(&ab).await["team"], team);
            drop(early);
            a.stop(&mut ca, &aa, &[]).await;
            (Node::start(cfg_a).await, b)
        }
    };
    let mut ca = Client::connect(&a.config).await;
    ca.attach(&aa).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    let status = ca.status(&aa).await;
    assert_eq!(status["team"], team);
    assert!(status["peers"].as_array().unwrap().is_empty());
    refused(&mut ca, to_removed("after-restart"), "send to removed").await;
    let address = cb.status(&ab).await["endpoint"].clone();
    refused(
        &mut ca,
        control(&aa, connect(address)),
        "connect to removed",
    )
    .await;
    // The removed peer keeps only its older checkpoint: no reset, no reform.
    // Its own stale view still admits a routing hint and a queued text; only
    // the remover's durable gate can refuse the session that would carry it.
    assert_eq!(cb.status(&ab).await["team"], team);
    let hint = cb.request(control(&ab, connect(status["endpoint"].clone())));
    assert_eq!(hint.await["result"]["status"], "ok");
    send(
        &mut cb,
        &ab,
        &a.config,
        "revoked-peer-attempt",
        "must not land",
    )
    .await;
    let reform = json!({"kind":"form","name":"silent reform"});
    refused(&mut cb, control(&ab, reform), "removed peer reform").await;
    b.stop(&mut cb, &ab, &[]).await;
    consume(&mut ca, &aa, &[]).await;
    assert!(ca.status(&aa).await["peers"].as_array().unwrap().is_empty());
    a.stop(&mut ca, &aa, &[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removal_holds_when_the_remover_restarts_before_the_removed_peer() {
    removal_survives_restart(Restart::RemoverFirst).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removal_holds_when_the_removed_peer_restarts_before_the_remover() {
    removal_survives_restart(Restart::RemovedFirst).await;
}
