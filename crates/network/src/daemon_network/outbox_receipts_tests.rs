use super::*;
use crate::{mail_store::TransportMessage, wire};
use cap_std::{ambient_authority, fs::Dir};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn fixture() -> (Root, Store, (EndpointId, u64), TransportMessage) {
    let peer = iroh::SecretKey::generate().public();
    let root = Root(std::env::temp_dir().join(format!("receipt-store-{peer}")));
    fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
    let dir = Dir::open_ambient_dir(&root.0, ambient_authority()).unwrap();
    let mut store = Store {
        mail: MailStore::open(&root.0).unwrap(),
        ledgers: BTreeMap::new(),
        receipts: BTreeMap::new(),
        _lease: DaemonLease::acquire(&dir).unwrap(),
    };
    let message = wire::Message {
        id: "exact-id".into(),
        from: "amber/desk/sender".into(),
        to: "birch/desk/reader".into(),
        body: "original immutable item".into(),
        sent_at: 100,
        wake: false,
        notify: false,
        from_agent: None,
        from_repo: None,
        via: None,
        reply_to: None,
        approval: None,
        files: Vec::new(),
    };
    let queued = store
        .mail
        .enqueue_transport("team", peer, &message)
        .unwrap();
    let key = (peer, 1);
    install(&mut store, key, &queued);
    (root, store, key, queued)
}
fn install(store: &mut Store, key: (EndpointId, u64), queued: &TransportMessage) {
    let mut ledger = OutboxLedger::new(key.0, key.1, crate::peer_session::MAX_PENDING).unwrap();
    assert!(matches!(
        ledger.prepare(&mut store.mail, queued).unwrap(),
        QueueAction::Send(_)
    ));
    store.ledgers.insert(key, ledger);
    store
        .receipts
        .insert(key, Arc::new(Mutex::new(Slot::default())));
}
fn sender(store: &Store, key: (EndpointId, u64)) -> ReceiptPublisher {
    publisher(store.receipts[&key].clone())
}
fn attempt(root: &Root) -> i64 {
    rusqlite::Connection::open_with_flags(
        root.0.join("mail.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT attempt FROM transport_outbox WHERE id='exact-id'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn retry_work_is_consumed_once_before_a_new_attempt_with_the_same_id() {
    let (root, mut store, key, queued) = fixture();
    let publish = sender(&store, key);
    let first = attempt(&root);
    publish("exact-id", &ReceiptDisposition::Retry, 7).unwrap();
    assert!(!store.drain(key, 7, true).unwrap());
    let retry = store.mail.transport_message("exact-id").unwrap().unwrap();
    assert!(retry.message == queued.message && retry.revision == queued.revision);
    assert!(!store.drain(key, 7, true).unwrap());
    assert!(matches!(
        store
            .ledgers
            .get_mut(&key)
            .unwrap()
            .prepare(&mut store.mail, &retry)
            .unwrap(),
        QueueAction::Send(_)
    ));
    assert_ne!(first, attempt(&root));
    publish(
        "exact-id",
        &ReceiptDisposition::Other("future-terminal".into()),
        7,
    )
    .unwrap();
    assert!(!store.drain(key, 7, true).unwrap());
    assert!(store.mail.transport_message("exact-id").unwrap().is_none());
    assert!(!store.drain(key, 7, true).unwrap());
}

#[test]
fn queued_receipt_cannot_remove_a_replacement_and_retired_publisher_cannot_reach_new_generation() {
    let (_root, mut store, key, queued) = fixture();
    let old_publish = sender(&store, key);
    old_publish("exact-id", &ReceiptDisposition::Queued, 7).unwrap();
    let mut message = queued.message.clone();
    message.body = "replacement item".into();
    let replacement = store
        .mail
        .enqueue_transport("team", key.0, &message)
        .unwrap();
    assert!(store.drain(key, 7, true).unwrap());
    assert!(
        store
            .mail
            .transport_message("exact-id")
            .unwrap()
            .unwrap()
            .message
            == message
    );
    store
        .ledgers
        .remove(&key)
        .unwrap()
        .disconnected(&mut store.mail)
        .unwrap();
    store.receipts.remove(&key);
    let current = (key.0, key.1 + 1);
    install(&mut store, current, &replacement);
    assert_eq!(
        old_publish("exact-id", &ReceiptDisposition::Queued, 7),
        Err(PeerError::Closed)
    );
    assert!(!store.drain(current, 7, true).unwrap());
    assert!(
        store
            .mail
            .transport_message("exact-id")
            .unwrap()
            .unwrap()
            .message
            == message
    );
    sender(&store, current)("exact-id", &ReceiptDisposition::Queued, 7).unwrap();
    assert!(!store.drain(current, 7, true).unwrap());
    assert!(store.mail.transport_message("exact-id").unwrap().is_none());
}

#[test]
fn revoked_or_epoch_changed_work_closes_its_sink_without_settling_the_durable_item() {
    for (active, revision) in [(false, 7), (true, 8)] {
        let (_root, mut store, key, queued) = fixture();
        let publish = sender(&store, key);
        publish("exact-id", &ReceiptDisposition::Queued, 7).unwrap();
        assert!(store.drain(key, revision, active).unwrap());
        assert_eq!(
            publish("exact-id", &ReceiptDisposition::Queued, 7),
            Err(PeerError::Closed)
        );
        let retained = store.mail.transport_message("exact-id").unwrap().unwrap();
        assert!(retained.message == queued.message && retained.revision == queued.revision);
    }
}
