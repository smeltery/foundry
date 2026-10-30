#![cfg(unix)]
// Shared first-party process helpers are read-only. Other helpers in their own
// broader process target are intentionally not called by this focused target.
#[path = "daemon_receipt_deadline/admission.rs"]
mod admission;
#[allow(dead_code)]
#[path = "daemon_network_process/child.rs"]
mod child;
#[allow(dead_code)]
#[path = "daemon_network_process/client.rs"]
mod client;
#[allow(dead_code)]
#[path = "daemon_network_process/clock.rs"]
mod clock;
#[allow(dead_code)]
#[path = "daemon_network_process/config.rs"]
mod config;
#[path = "daemon_receipt_deadline/observations.rs"]
mod observations;
#[path = "daemon_receipt_deadline/peer.rs"]
mod peer;
use serde_json::json;
use std::time::{Duration, Instant};
const DEADLINE: Duration = Duration::from_secs(45);
const ID: &str = "01J00000000000000000000001";
const BODY: &str = "immutable application receipt deadline fixture";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_peer_withholding_ack_is_retired_after_five_seconds_then_replay_deduplicates() {
    receipt_replay(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_permitted_replay_receipt_is_acked_on_a_later_connection_without_new_permission() {
    receipt_replay(true).await;
}

async fn receipt_replay(lose_replay: bool) {
    peer::diagnostic(format_args!("test_start lose_replay={}", lose_replay));
    let root = config::Root::new();
    let mut sender_config = root.config("sender", "alex");
    // Isolate the receipt deadline from unrelated invitation-effect revisions.
    sender_config.value["network"]["codebook"] = serde_json::Value::Null;
    let mut sender = child::Node::start(sender_config).await;
    peer::diagnostic(format_args!(
        "sender_start lose_replay={} daemon_pid={:?}",
        lose_replay,
        sender.diagnostic_pid()
    ));
    let (mut ipc, auth) = sender.client().await;
    ipc.control(
        &auth,
        json!({"kind":"form","name":"receipt deadline fixture"}),
    )
    .await;
    let recipient = root.config("controlled", "blair");
    let admitted = admission::Admitted::join(&mut ipc, &auth, &recipient).await;
    let ticket = admitted.ticket.clone();
    let bearer = admitted.bearer.clone();
    let mut peer = if lose_replay {
        peer::Controlled::start_with_lost_replay_receipt(admitted)
    } else {
        peer::Controlled::start(admitted)
    };
    ipc.control(&auth, json!({"kind":"connect","address":peer.address}))
        .await;
    tokio::time::timeout(DEADLINE, async {
        loop {
            let who = ipc
                .ok(json!({"op":"who","filter":recipient.destination()}))
                .await;
            if who["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["online"] == true)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("actual authenticated presence before send");
    let send_started = Instant::now();
    let queued = ipc
        .ok(json!({
            "op": "send", "auth": auth, "message_id": ID,
            "to": recipient.destination(), "body": BODY
        }))
        .await;
    assert_eq!(queued["kind"], "transport_queued");
    let (original, first_connection) = match peer.event().await {
        peer::Event::Message {
            payload,
            connection,
            duplicate: false,
        } => (payload, connection),
        _ => panic!("first actual wire message required"),
    };
    let source = sender.config.state.join("mail.sqlite3");
    let received = recipient.state.join("mail.sqlite3");
    let first =
        observations::claim(&source, ID).expect("withheld wire ACK must leave outbox pending");
    assert!(first.payload == original);
    assert!(first.attempt.is_some() && first.generation.is_some());
    observations::receiver(&received, ID, &original);
    let closed = tokio::time::timeout(Duration::from_secs(12), peer.event())
        .await
        .expect("five-second receipt deadline did not retire a live transport");
    match closed {
        peer::Event::Closed {
            connection,
            elapsed,
            heartbeats,
        } => {
            assert_eq!(connection, first_connection);
            assert!(
                send_started.elapsed() >= Duration::from_secs(5),
                "retired before five seconds from local send"
            );
            assert!(
                elapsed >= Duration::from_secs(4),
                "retired before receipt deadline"
            );
            assert!(
                elapsed <= Duration::from_secs(12),
                "not a bounded five-second receipt retirement"
            );
            assert!(
                heartbeats >= 8,
                "withheld receipt did not keep sending valid presence"
            );
        }
        _ => panic!("daemon must close old connection before actual replay"),
    }
    // Automatic reconnect may already claim a new attempt; the old owner must
    // disappear without dropping or changing the unacknowledged envelope.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = observations::claim(&source, ID)
                .expect("retirement must retain the unacknowledged row");
            assert!(current.payload == original);
            assert_eq!(current.revision, first.revision);
            if current.attempt != first.attempt {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("old durable attempt must retire before replay is observed");
    let (replay, replay_connection) = match peer.event().await {
        peer::Event::Message {
            payload,
            connection,
            duplicate: true,
        } => {
            assert_ne!(connection, first_connection, "replay reused old transport");
            (payload, connection)
        }
        _ => panic!("new connection must replay and durably deduplicate"),
    };
    assert!(
        replay == original,
        "full immutable wire payload changed on retry"
    );
    let second = observations::claim(&source, ID).expect("no ACK yet on replay transport");
    assert!(second.payload == original);
    assert_eq!(second.revision, first.revision);
    assert_ne!(second.attempt, first.attempt);
    assert_ne!(second.generation, first.generation);
    observations::receiver(&received, ID, &original);
    peer::diagnostic(format_args!(
        "second_claim attempt={:?} generation={:?} revision={}",
        second.attempt, second.generation, second.revision
    ));
    peer.acknowledge_replay().await;
    if lose_replay {
        match peer.event().await {
            peer::Event::ReceiptLost { connection } => assert_eq!(connection, replay_connection),
            _ => panic!("permitted replay receipt must be deliberately lost"),
        }
    }
    match peer.event().await {
        peer::Event::AckWritten { connection } => {
            assert_ne!(connection, first_connection);
            if lose_replay {
                assert_ne!(connection, replay_connection);
            }
        }
        _ => panic!("actual receipt write on a current replay transport required"),
    }
    tokio::time::timeout(DEADLINE, async {
        let mut previous = None;
        let mut previous_running = true;
        while let Some(row) = observations::claim(&source, ID) {
            let running = sender.diagnostic_running();
            if running != previous_running {
                peer::diagnostic(format_args!(
                    "sender_liveness lose_replay={} running={}",
                    lose_replay, running
                ));
                previous_running = running;
            }
            let current = (row.attempt, row.generation, row.revision);
            if previous.as_ref() != Some(&current) {
                peer::diagnostic(format_args!(
                    "queue attempt={:?} generation={:?} revision={}",
                    current.0, current.1, current.2
                ));
                previous = Some(current);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("actual replay wire ACK did not settle exact outbox item");
    observations::receiver(&received, ID, &original);
    let credential = serde_json::to_string(&auth["credential"]).unwrap();
    sender
        .stop(&mut ipc, &auth, &[&ticket, &bearer, &credential, BODY])
        .await;
    peer.shutdown().await;
}
