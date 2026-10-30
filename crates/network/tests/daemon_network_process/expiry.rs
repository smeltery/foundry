use super::{DEADLINE, child::Node, client::Client, clock::Clock, config::Root, flow::*};
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_code_expiry_retires_publication_and_refuses_late_join_without_editing_storage() {
    let root = Root::new();
    let clock = Clock::build(&root.0);
    let mut cfg_a = root.config("a", "alex");
    cfg_a.clock = Some(clock.clone());
    let a = Node::start(cfg_a).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, true).await;
    let code = invitation.code.as_ref().unwrap();
    let intent = invitation.intent.as_ref().unwrap();
    let endpoint = invitation.code_endpoint.as_ref().unwrap();
    // Only these child processes see +2h+1s. Tokio and fixture deadlines retain
    // real monotonic time, and the earlier1h invitation keeps its actual bytes.
    clock.set(7201);
    tokio::time::timeout(DEADLINE, async {
        loop {
            let value = ca.control(&aa, json!({"kind":"code_publications"})).await;
            assert_eq!(value["kind"], "code_publications");
            if value["publications"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["intent"] != *intent)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("expired real listener publication retirement");
    let mut cfg_b = root.config("b", "blair");
    cfg_b.clock = Some(clock.clone());
    let b = Node::start(cfg_b).await;
    let (mut cb, ab) = b.client().await;
    let reply = cb
        .request(json!({"op":"network","auth":ab,
        "command":{"kind":"join_code","code":code,"address":endpoint}}))
        .await;
    assert_eq!(reply["result"]["status"], "error");
    assert!(cb.status(&ab).await["team"].is_null());
    b.stop(&mut cb, &ab, &[code]).await;
    a.stop(&mut ca, &aa, &[code, &invitation.ticket]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consumed_startup_ticket_reopens_real_active_checkpoint_after_its_clock_expiry() {
    let root = Root::new();
    let clock = Clock::build(&root.0);
    let mut cfg_a = root.config("a", "alex");
    cfg_a.clock = Some(clock.clone());
    let a = Node::start(cfg_a).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, false).await;
    let mut cfg_b = root.config("b", "blair");
    cfg_b.clock = Some(clock.clone());
    seed(&mut cfg_b, &invitation.endpoint);
    cfg_b.value["network"]["startup"] = json!({"mode":"join_ticket","ticket":invitation.ticket});
    let b = Node::start(cfg_b).await;
    let (mut cb, ab) = b.client().await;
    let team = cb.status(&ab).await["team"].clone();
    assert_eq!(team, ca.status(&aa).await["team"]);
    let cfg_b = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    clock.set(7201);
    let b = Node::start(cfg_b).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    assert_eq!(cb.status(&ab).await["team"], team);
    // A fresh unjoined process still cannot redeem the same expired ticket.
    let mut cfg_c = root.config("c", "casey");
    cfg_c.clock = Some(clock.clone());
    let c = Node::start(cfg_c).await;
    let (mut cc, ac) = c.client().await;
    let refused = cc
        .request(json!({"op":"network","auth":ac,
        "command":{"kind":"join","ticket":invitation.ticket,"address":invitation.endpoint}}))
        .await;
    assert_eq!(refused["result"]["status"], "error");
    assert!(cc.status(&ac).await["team"].is_null());
    c.stop(&mut cc, &ac, &[&invitation.ticket]).await;
    b.stop(&mut cb, &ab, &[&invitation.ticket]).await;
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
}
