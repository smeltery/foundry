#![cfg(unix)]
use foundry_network::{mail_store::*, wire};
use iroh::{EndpointId, SecretKey};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "transport-test-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
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
    SecretKey::from_bytes(&[seed; 32]).public()
}
fn message(id: &str, body: &str, sent_at: i64) -> wire::Message {
    wire::Message {
        id: id.into(),
        from: "alice/local/terminal".into(),
        from_agent: Some("client".into()),
        from_repo: Some("project".into()),
        to: "bob/remote/reader".into(),
        via: None,
        reply_to: Some("thread".into()),
        body: body.into(),
        wake: true,
        notify: false,
        sent_at,
        approval: None,
        files: Vec::new(),
    }
}
#[test]
fn replacement_global_id_order_timestamp_and_exact_receipt_cas() {
    let root = Root::new();
    let mut store = root.open();
    let a = peer(1);
    let b = peer(2);
    let first = store
        .enqueue_transport("team", a, &message("z", "original", 100))
        .unwrap();
    let old = store.begin_transport_attempt(&first, 1).unwrap();
    store
        .enqueue_transport("team", a, &message("a", "second", 100))
        .unwrap();
    let replacement = store
        .enqueue_transport("team", a, &message("z", "replacement", 100))
        .unwrap();
    assert!(replacement.revision > first.revision);
    assert!(matches!(
        store.finish_transport_attempt(&old, a, 1, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    let ordered = store.transport_for("team", a, 100).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|q| q.message.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    let active = store.begin_transport_attempt(&replacement, 2).unwrap();
    assert!(matches!(
        store.finish_transport_attempt(&active, b, 2, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    assert!(matches!(
        store.finish_transport_attempt(&active, a, 1, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    let mut forged = replacement.clone();
    forged.message.body = "different".into();
    assert!(matches!(
        store.begin_transport_attempt(&forged, 3),
        Err(MailError::StaleAttempt)
    ));
    assert!(matches!(
        store.begin_transport_attempt(&replacement, 1),
        Err(MailError::StaleAttempt)
    ));
    store
        .finish_transport_attempt(&active, a, 2, TransportDisposition::Queued)
        .unwrap();
    assert!(matches!(
        store.finish_transport_attempt(&active, a, 2, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    let moved = store
        .enqueue_transport("other-team", b, &message("a", "retargeted explicitly", 50))
        .unwrap();
    assert!(store.transport_for("team", a, 100).unwrap().is_empty());
    assert_eq!(
        store.transport_for("other-team", b, 100).unwrap()[0].revision,
        moved.revision
    );
}
#[test]
fn source_open_receipt_states_retry_terminal_and_raw_notification_spelling() {
    let root = Root::new();
    let mut store = root.open();
    let peer = peer(3);
    for state in [
        "queued",
        "delivered",
        "held",
        "refused",
        "retry",
        "notified",
        "not notified",
        "future-state",
        "",
        "RETRY",
    ] {
        let disposition = TransportDisposition::from_state(state).unwrap();
        assert_eq!(disposition.as_state(), state);
        assert_eq!(disposition.taken(), state != "refused" && state != "retry");
        let queued = store
            .enqueue_transport("team", peer, &message("same", "body", 0))
            .unwrap();
        let attempt = store.begin_transport_attempt(&queued, u64::MAX).unwrap();
        assert_eq!(attempt.generation(), u64::MAX);
        assert_eq!(attempt.message().message.sent_at, 0);
        store
            .finish_transport_attempt(&attempt, peer, u64::MAX, disposition)
            .unwrap();
        assert_eq!(
            store.transport_for("team", peer, 0).unwrap().len(),
            usize::from(state == "retry")
        );
    }
    let queued = store
        .enqueue_transport("team", peer, &message("same", "body", 0))
        .unwrap();
    let attempt = store.begin_transport_attempt(&queued, 1).unwrap();
    store
        .finish_transport_attempt(
            &attempt,
            peer,
            1,
            TransportDisposition::Other("retry".into()),
        )
        .unwrap();
    assert_eq!(store.transport_for("team", peer, 0).unwrap().len(), 1);
    assert!(!TransportDisposition::Other("refused".into()).taken());
}
#[test]
fn strict_week_expiry_is_global_and_restart_replay_keeps_full_envelope() {
    let root = Root::new();
    let a = peer(4);
    let b = peer(5);
    let mut store = root.open();
    let original = message("old", "unchanged", 0);
    let queued = store.enqueue_transport("team", a, &original).unwrap();
    let attempt = store.begin_transport_attempt(&queued, 8).unwrap();
    drop(store);
    let mut store = root.open();
    store.recover_transport().unwrap();
    assert!(matches!(
        store.finish_transport_attempt(&attempt, a, 8, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    let replay = store
        .transport_for("team", a, TRANSPORT_RETENTION_MS)
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].message, original);
    store
        .enqueue_transport("other", b, &message("other", "retained", 1))
        .unwrap();
    assert!(
        store
            .transport_for("missing", b, TRANSPORT_RETENTION_MS + 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .transport_for("team", a, TRANSPORT_RETENTION_MS + 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .transport_for("other", b, TRANSPORT_RETENTION_MS + 1)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn disconnect_only_retires_exact_peer_generation_and_does_not_consume_inbox() {
    let root = Root::new();
    let mut store = root.open();
    let a = peer(6);
    let b = peer(7);
    let inbox = Message {
        id: "inbox".into(),
        sender: "sender".into(),
        recipient: "recipient".into(),
        body: b"body".to_vec(),
        encoding: PayloadEncoding::Text,
        reply_to: None,
        intent: DeliveryIntent::default(),
    };
    store.insert(&inbox, State::Pending).unwrap();
    let one = store
        .enqueue_transport("team", a, &message("one", "one", 1))
        .unwrap();
    let two = store
        .enqueue_transport("team", b, &message("two", "two", 1))
        .unwrap();
    let one = store.begin_transport_attempt(&one, 10).unwrap();
    let two = store.begin_transport_attempt(&two, 10).unwrap();
    store.disconnect_transport(a, 9).unwrap();
    store.disconnect_transport(a, 10).unwrap();
    assert!(matches!(
        store.finish_transport_attempt(&one, a, 10, TransportDisposition::Queued),
        Err(MailError::StaleAttempt)
    ));
    store
        .finish_transport_attempt(&two, b, 10, TransportDisposition::Refused)
        .unwrap();
    assert_eq!(store.transport_for("team", a, 1).unwrap().len(), 1);
    assert_eq!(store.unread("recipient").unwrap(), 1);
    assert_eq!(store.get("inbox").unwrap().state, State::Pending);
}
#[test]
fn corrupt_later_transport_json_is_error_instead_of_filtered_success() {
    let root = Root::new();
    let mut store = root.open();
    let peer = peer(8);
    for id in ["one", "two"] {
        store
            .enqueue_transport("team", peer, &message(id, "body", 1))
            .unwrap();
    }
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    raw.execute(
        "UPDATE transport_outbox SET payload=x'7b' WHERE id='two'",
        [],
    )
    .unwrap();
    assert!(store.transport_for("team", peer, 1).is_err());
    assert_eq!(
        raw.query_row("SELECT count(*) FROM transport_outbox", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
#[test]
fn exact_v3_additive_upgrade_preserves_inbox_peek_and_sequence() {
    let root = Root::new();
    let mut store = root.open();
    for (id, state) in [
        ("pending", State::Pending),
        ("held", State::Held),
        ("rejected", State::Rejected),
    ] {
        store
            .insert(
                &Message {
                    id: id.into(),
                    sender: "sender".into(),
                    recipient: "reader".into(),
                    body: id.as_bytes().to_vec(),
                    encoding: PayloadEncoding::Text,
                    reply_to: Some("reply".into()),
                    intent: DeliveryIntent {
                        wake: false,
                        notify: true,
                    },
                },
                state,
            )
            .unwrap();
    }
    let pending = store.poll_batch("reader", "old-session", 5, true).unwrap();
    let attempt = pending[0].attempt.clone().unwrap();
    drop(store);
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    // Original empty new tables are removed only to construct an exact v3 test
    // fixture. Production migration never drops or rewrites any prior table.
    raw.execute_batch("DROP TABLE transport_outbox; DROP TABLE remote_members; DROP TABLE remote_presence; DROP TABLE remote_epoch; PRAGMA user_version=3;").unwrap();
    let before: i64 = raw
        .query_row("SELECT value FROM sequence", [], |r| r.get(0))
        .unwrap();
    let mut reopened = root.open();
    assert_eq!(
        raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(reopened.get("held").unwrap().state, State::Held);
    assert_eq!(reopened.get("rejected").unwrap().state, State::Rejected);
    assert_eq!(reopened.get("pending").unwrap().message.body, b"pending");
    reopened.acknowledge(&attempt, "old-session", 6).unwrap();
    assert_eq!(reopened.get("pending").unwrap().state, State::Delivered);
    reopened
        .enqueue_transport("team", peer(9), &message("new", "queued", 8))
        .unwrap();
    assert_eq!(reopened.transport_for("team", peer(9), 8).unwrap().len(), 1);
}
#[test]
fn transport_quota_failure_is_atomic_and_replacement_uses_net_bytes() {
    let root = Root::new();
    let mut store = root.open();
    let peer = peer(10);
    let body = "a".repeat(4 * 1024 * 1024 - 4096);
    for n in 0..16 {
        store
            .enqueue_transport("team", peer, &message(&format!("q{n}"), &body, 1))
            .unwrap();
    }
    assert!(matches!(
        store.enqueue_transport("team", peer, &message("overflow", &body, 1)),
        Err(MailError::Capacity)
    ));
    let smaller = store
        .enqueue_transport("team", peer, &message("q0", "small", 1))
        .unwrap();
    assert_eq!(smaller.message.body, "small");
    let queue = store.transport_for("team", peer, 1).unwrap();
    assert_eq!(queue.len(), 16);
    assert!(!queue.iter().any(|q| q.message.id == "overflow"));
}
#[path = "mail_store_transport/edges.rs"]
mod edges;
