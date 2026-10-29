#![cfg(unix)]
use foundry_network::peer_session;
pub use foundry_network::{
    Channel, NetworkEndpoint, NetworkError, PeerFrame, WireProfile, daemon_lease, full_team_store,
    mail_store, read_frame, signing, team, wire, write_frame,
};
use peer_session::*;

use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
#[path = "peer_session/helpers.rs"]
mod helpers;
use helpers::*;

#[tokio::test]
async fn actual_two_peer_state_mail_dedup_reconnect_and_receipt_ack() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    // State created after provisioning must reach B through a durable merge.
    let mut store = a.store();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_rotation(&base, NOW + 1).unwrap();
    store.commit_transition(pending, NOW + 1).unwrap();
    let expected = store.load(NOW + 1).unwrap().document().unwrap().clone();
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (auth_a, auth_b) = (a.authority(false), b.authority(true));
    let (mut sender, mut receiver) =
        pair(&left, &right, auth_a.clone(), auth_b.clone(), &cancel).await;
    assert_eq!(sender.peer(), right.id());
    assert!(b.store().load(NOW + 100).unwrap().document() == Some(&expected));
    sender.send_text(message()).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Received { duplicate: false, message_id } if message_id == "receipt-1"
    ));
    // Lose the application receipt ACK by closing without reading it.
    drop((sender, receiver));
    let (mut sender, mut receiver) = pair(&left, &right, auth_a, auth_b, &cancel).await;
    sender.send_text(message()).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Received {
            duplicate: true,
            ..
        }
    ));
    assert!(
        matches!(sender.receive().await.unwrap(), PeerEvent::Queued { message_id } if message_id == "receipt-1")
    );
    let db = foundry_network::mail_store::MailStore::open(&b.root).unwrap();
    assert_eq!(db.unread("bob/desktop/terminal").unwrap(), 1);
    let record = db.get("receipt-1").unwrap();
    assert_eq!(
        serde_json::from_slice::<wire::Message>(&record.message.body).unwrap(),
        message()
    );
    assert_eq!(record.state, foundry_network::mail_store::State::Pending);
    drop((sender, receiver, db));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn unadmitted_transport_peer_cannot_acquire_state() {
    let (a, b, outsider) = (Node::new(), Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (outsider.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (client, server) = tokio::join!(
        left.connect(right.address(), Channel::Team, WAIT),
        PeerSession::accept(&right, b.authority(false), options(&cancel))
    );
    assert!(matches!(server, Err(PeerError::Unauthorized)));
    client.unwrap().close(0u32.into(), b"fixture finished");
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn authenticated_peer_wrong_hello_team_is_refused() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let hello = PeerFrame::Hello {
        team: "other-team".into(),
        proto: 1,
    };
    assert!(matches!(
        raw(&left, &right, b.authority(false), &cancel, hello).await,
        Err(PeerError::DifferentTeam)
    ));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn removal_fences_existing_session_and_reconnect() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (auth_a, auth_b) = (a.authority(false), b.authority(false));
    let (mut sender, receiver) = pair(&left, &right, auth_a.clone(), auth_b.clone(), &cancel).await;
    auth_a.remove(vec![b.identity.endpoint_id()]).await.unwrap();
    assert_eq!(sender.publish_state().await, Err(PeerError::Unauthorized));
    drop((sender, receiver));
    let (outgoing, incoming) = tokio::join!(
        PeerSession::connect(&left, right.address(), auth_a, options(&cancel)),
        PeerSession::accept(&right, auth_b, options(&cancel))
    );
    assert!(matches!(outgoing, Err(PeerError::Unauthorized)));
    assert!(incoming.is_err());
    tokio::join!(left.shutdown(), right.shutdown());
}

#[path = "peer_session/failures.rs"]
mod failures;
#[path = "peer_session/lifecycle.rs"]
mod lifecycle;

#[test]
fn constructor_rejects_another_store_root_or_admitted_local_identity() {
    use cap_std::{ambient_authority, fs::Dir};
    let (a, b, other) = (Node::new(), Node::new(), Node::new());
    provision(&a, &b);
    for (root, identity) in [
        (&other.root, a.identity.endpoint_id()),
        (&a.root, b.identity.endpoint_id()),
    ] {
        let directory = Dir::open_ambient_dir(root, ambient_authority()).unwrap();
        let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
        let result = AdmittedTeam::new(a.store(), ownership, identity, || Ok(NOW), None);
        assert!(matches!(result, Err(PeerError::Unauthorized)));
        assert!(daemon_lease::DaemonLease::acquire(&directory).is_ok());
    }
}

#[path = "peer_session/codes.rs"]
mod codes;
#[path = "peer_session/join_gate.rs"]
mod join_gate;
#[path = "peer_session/reader.rs"]
mod reader;

#[path = "peer_session/code_bootstrap.rs"]
mod code_bootstrap;
#[path = "peer_session/driver.rs"]
mod driver;
#[path = "peer_session/engine_inbox.rs"]
mod engine_inbox;
