#[allow(dead_code)]
#[path = "../handshake_hold/mod.rs"]
mod handshake_hold;

use super::{child::Node, client::Client, config::Root, flow::*};
use foundry_network::full_team_store::FULL_TEAM_LOCK;
use handshake_hold::HandshakeHold;
use serde_json::json;
use std::{
    fs::File,
    time::{Duration, Instant},
};

const SETTLE: Duration = Duration::from_millis(500);

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
    // Every team-store operation takes this lock file before SQLite. Never lock
    // the WAL store itself: another process must not open it while it is live.
    let store_lock = File::open(b.config.state.join(FULL_TEAM_LOCK)).unwrap();
    // JOIN reads the unjoined store before dialing. A handshake held mid-flight
    // proves that read finished, so the lock blocks only the local commit.
    let hold = HandshakeHold::start(&invitation.endpoint).await;
    cb.send(json!({"op":"network","auth":ab,"command":{"kind":"join",
        "ticket":invitation.ticket,"address":hold.address(&invitation.endpoint)}}))
        .await;
    tokio::time::timeout(super::DEADLINE, hold.stalled())
        .await
        .expect("JOIN handshake reached the inviter");
    store_lock.lock().unwrap();
    let blocked = Instant::now();
    hold.release();
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
    // Unblocked, the joiner commits within milliseconds of admission. Waiting
    // here turns a commit that the lock failed to hold into a visible failure.
    tokio::time::sleep(SETTLE).await;
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
    .expect("IPC cancellation must close before store busy failure");
    // FullTeamStore uses the runtime's 10s lock/busy deadline. Do not let its
    // timeout substitute for evidence that EOF fenced the pending commit.
    assert!(blocked.elapsed() < Duration::from_secs(5));
    drop(store_lock);
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
