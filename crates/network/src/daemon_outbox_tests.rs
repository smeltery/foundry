use super::*;
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("outbox-{}", iroh::SecretKey::generate().public()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> MailStore {
        MailStore::open(&self.0).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn peer(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}
fn message(id: &str, body: &str) -> wire::Message {
    wire::Message {
        id: id.into(),
        from: "alice/local/terminal".into(),
        from_agent: Some("client".into()),
        from_repo: Some("project".into()),
        to: "bob/remote/reader".into(),
        via: None,
        reply_to: None,
        body: body.into(),
        wake: false,
        notify: false,
        sent_at: 100,
        approval: None,
        files: Vec::new(),
    }
}
fn expect_send(action: QueueAction, body: &str) {
    match action {
        QueueAction::Send(message) => assert_eq!(message.body, body),
        _ => panic!("expected durable send"),
    }
}

#[test]
fn retry_replays_original_and_terminal_receipt_preserves_reuse_fence() {
    let root = Root::new();
    let mut store = root.open();
    let queued = store
        .enqueue_transport("team", peer(1), &message("id", "original"))
        .unwrap();
    let mut ledger = OutboxLedger::new(peer(1), 1, 2).unwrap();
    expect_send(ledger.prepare(&mut store, &queued).unwrap(), "original");
    assert!(matches!(
        ledger.prepare(&mut store, &queued).unwrap(),
        QueueAction::Pending
    ));
    assert!(matches!(
        ledger
            .settle(&mut store, "id", TransportDisposition::Retry)
            .unwrap(),
        Settlement::Settled { taken: false }
    ));
    assert!(store.transport_message("id").unwrap().is_some());
    expect_send(ledger.prepare(&mut store, &queued).unwrap(), "original");
    assert!(matches!(
        ledger
            .settle(
                &mut store,
                "id",
                TransportDisposition::Other("future-state".into())
            )
            .unwrap(),
        Settlement::Settled { taken: true }
    ));
    assert!(store.transport_message("id").unwrap().is_none());
    assert!(!ledger.requires_retirement(&store).unwrap());
    let reused = store
        .enqueue_transport("team", peer(1), &message("id", "replacement"))
        .unwrap();
    assert!(matches!(
        ledger.prepare(&mut store, &reused).unwrap(),
        QueueAction::Retire
    ));
    assert!(matches!(
        ledger
            .settle(&mut store, "id", TransportDisposition::Queued)
            .unwrap(),
        Settlement::Retire
    ));
    assert_eq!(
        store.transport_message("id").unwrap().unwrap().message.body,
        "replacement"
    );
}

#[test]
fn replacement_onto_another_peer_retires_old_connection_before_ack() {
    let root = Root::new();
    let mut store = root.open();
    let original = store
        .enqueue_transport("team", peer(1), &message("id", "original"))
        .unwrap();
    let mut old = OutboxLedger::new(peer(1), 10, 2).unwrap();
    expect_send(old.prepare(&mut store, &original).unwrap(), "original");
    let replacement = store
        .enqueue_transport("other-team", peer(2), &message("id", "replacement"))
        .unwrap();
    assert!(old.requires_retirement(&store).unwrap());
    assert!(matches!(
        old.settle(&mut store, "id", TransportDisposition::Queued)
            .unwrap(),
        Settlement::Retire
    ));
    old.disconnected(&mut store).unwrap();
    let mut current = OutboxLedger::new(peer(2), 11, 2).unwrap();
    expect_send(
        current.prepare(&mut store, &replacement).unwrap(),
        "replacement",
    );
    assert!(matches!(
        current
            .settle(&mut store, "id", TransportDisposition::Refused)
            .unwrap(),
        Settlement::Settled { taken: false }
    ));
    assert!(store.transport_message("id").unwrap().is_none());
}

#[test]
fn dropped_receipt_and_restart_replay_without_reusing_old_attempt() {
    let root = Root::new();
    let mut store = root.open();
    let queued = store
        .enqueue_transport("team", peer(1), &message("id", "original"))
        .unwrap();
    let mut old = OutboxLedger::new(peer(1), 7, 1).unwrap();
    expect_send(old.prepare(&mut store, &queued).unwrap(), "original");
    old.disconnected(&mut store).unwrap();
    drop(store);
    let mut reopened = root.open();
    reopened.recover_transport().unwrap();
    let replay = reopened
        .transport_for("team", peer(1), 101)
        .unwrap()
        .remove(0);
    let mut current = OutboxLedger::new(peer(1), 8, 1).unwrap();
    expect_send(current.prepare(&mut reopened, &replay).unwrap(), "original");
    reopened.disconnect_transport(peer(1), 7).unwrap();
    assert!(matches!(
        current
            .settle(&mut reopened, "id", TransportDisposition::Queued)
            .unwrap(),
        Settlement::Settled { taken: true }
    ));
}

#[test]
fn expired_pending_attempt_cannot_settle_replacement_or_starve_a_new_driver() {
    let root = Root::new();
    let mut store = root.open();
    let queued = store
        .enqueue_transport("team", peer(1), &message("id", "old"))
        .unwrap();
    let mut old = OutboxLedger::new(peer(1), 1, 1).unwrap();
    expect_send(old.prepare(&mut store, &queued).unwrap(), "old");
    assert!(
        store
            .transport_for(
                "team",
                peer(1),
                101 + crate::mail_store::TRANSPORT_RETENTION_MS
            )
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        old.settle(&mut store, "id", TransportDisposition::Queued)
            .unwrap(),
        Settlement::Retire
    ));
    let replacement = store
        .enqueue_transport("team", peer(1), &message("id", "new"))
        .unwrap();
    old.disconnected(&mut store).unwrap();
    let mut current = OutboxLedger::new(peer(1), 2, 1).unwrap();
    expect_send(current.prepare(&mut store, &replacement).unwrap(), "new");
}

#[test]
fn missing_receipt_has_finite_deadline_and_retry_retires_only_its_attempt() {
    let root = Root::new();
    let mut store = root.open();
    let queued = store
        .enqueue_transport("team", peer(1), &message("id", "original"))
        .unwrap();
    let mut ledger = OutboxLedger::new(peer(1), 1, 1).unwrap();
    expect_send(ledger.prepare(&mut store, &queued).unwrap(), "original");
    let now = std::time::Instant::now();
    assert!(!ledger.receipt_deadline_elapsed(now));
    assert!(ledger.receipt_deadline_elapsed(now + RECEIPT_TIMEOUT));
    assert!(store.transport_message("id").unwrap().is_some());
    assert!(matches!(
        ledger
            .settle(&mut store, "id", TransportDisposition::Retry)
            .unwrap(),
        Settlement::Settled { taken: false }
    ));
    assert!(!ledger.receipt_deadline_elapsed(now + RECEIPT_TIMEOUT));
    expect_send(ledger.prepare(&mut store, &queued).unwrap(), "original");
    ledger.disconnected(&mut store).unwrap();
    let mut replacement = OutboxLedger::new(peer(1), 2, 1).unwrap();
    expect_send(
        replacement.prepare(&mut store, &queued).unwrap(),
        "original",
    );
    assert!(matches!(
        replacement
            .settle(&mut store, "id", TransportDisposition::Queued)
            .unwrap(),
        Settlement::Settled { taken: true }
    ));
}
