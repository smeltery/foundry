use super::{child::Node, client::Client, config::Root, database::WriteLock, flow::*};
use foundry_network::full_team_store::FULL_TEAM_DATABASE;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn half_closed_join_control_cannot_commit_after_remote_admission_and_local_write_block() {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, false).await;
    let b = Node::start(root.config("b", "blair")).await;
    let (mut cb, ab) = b.client().await;
    let endpoint: iroh::EndpointAddr =
        serde_json::from_value(cb.status(&ab).await["endpoint"].clone()).unwrap();
    let blocked = WriteLock::new(&b.config.state.join(FULL_TEAM_DATABASE));
    cb.send(json!({"op":"network","auth":ab,"command":{"kind":"join",
        "ticket":invitation.ticket,"address":invitation.endpoint}}))
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let peers = ca.status(&aa).await["peers"].clone();
            if peers
                .as_array()
                .unwrap()
                .iter()
                .any(|peer| peer == &json!(endpoint.id))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("remote admission before pending local commit");
    cb.shutdown_write().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(reply) = cb.receive().await {
            assert_eq!(
                reply["result"]["status"], "error",
                "cancelled JOIN returned success"
            );
        }
    })
    .await
    .expect("IPC cancellation must close before SQLite busy failure");
    // FullTeamStore uses the runtime's10s open/busy deadline. Do not let its
    // timeout substitute for evidence that EOF fenced the pending commit.
    assert!(blocked.elapsed() < Duration::from_secs(5));
    drop(blocked);
    let mut inspect = Client::connect(&b.config).await;
    inspect.attach(&ab).await;
    assert!(
        inspect.status(&ab).await["team"].is_null(),
        "cancelled JOIN committed authority"
    );
    let config = b.config.clone();
    b.stop(&mut inspect, &ab, &[]).await;
    let b = Node::start(config).await;
    let mut inspect = Client::connect(&b.config).await;
    inspect.attach(&ab).await;
    assert!(inspect.status(&ab).await["team"].is_null());
    b.stop(&mut inspect, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
}
