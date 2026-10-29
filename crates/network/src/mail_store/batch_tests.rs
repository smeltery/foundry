use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("batch-{}", data_encoding::HEXLOWER.encode(&nonce)));
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
fn message(id: &str, recipient: &str) -> Message {
    Message {
        id: id.into(),
        sender: "sender".into(),
        recipient: recipient.into(),
        body: b"body".to_vec(),
        encoding: PayloadEncoding::Text,
        reply_to: None,
        intent: DeliveryIntent::default(),
    }
}
#[test]
fn later_invalid_payload_rolls_back_all_earlier_consumption() {
    let root = Root::new();
    let mut store = root.open();
    for (id, to) in [("first", "a"), ("bad", "a"), ("other", "b")] {
        store.insert(&message(id, to), State::Pending).unwrap();
    }
    let raw = Connection::open(root.0.join("mail.sqlite3")).unwrap();
    raw.execute("UPDATE mail SET encoding=1 WHERE id='bad'", [])
        .unwrap();
    for peek in [false, true] {
        assert!(matches!(
            store.poll_batch("a", "s", 0, peek),
            Err(MailError::InvalidPayload)
        ));
        assert_eq!(store.unread("a").unwrap(), 2);
        assert_eq!(store.get("first").unwrap().state, State::Pending);
    }
    assert_eq!(store.unread("b").unwrap(), 1);
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn cancellation_at_commit_gate_rolls_back_whole_batch() {
    let root = Root::new();
    let mut store = root.open();
    for id in ["one", "two"] {
        store.insert(&message(id, "a"), State::Pending).unwrap();
    }
    for peek in [false, true] {
        let checks = AtomicUsize::new(0);
        let result = store
            .poll_batch_checked(
                "a",
                "s",
                0,
                peek,
                |reads| {
                    assert_eq!(reads.len(), 2);
                    Ok(())
                },
                || checks.fetch_add(1, Ordering::SeqCst) < 2,
            )
            .unwrap();
        assert!(result.is_none());
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert_eq!(store.unread("a").unwrap(), 2);
        let raw = Connection::open(root.0.join("mail.sqlite3")).unwrap();
        assert_eq!(
            raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let reads = store.poll_batch("a", "s", 1, false).unwrap();
    assert_eq!(reads.len(), 2);
    assert_eq!(store.unread("a").unwrap(), 0);
}
#[test]
fn preparation_failure_and_message_limit_never_truncate_or_allocate_tokens() {
    let root = Root::new();
    let mut store = root.open();
    for id in ["one", "two"] {
        store.insert(&message(id, "a"), State::Pending).unwrap();
    }
    assert!(matches!(
        store.poll_batch_checked(
            "a",
            "s",
            0,
            true,
            |_| Err::<(), _>(MailError::Capacity),
            || true
        ),
        Err(MailError::Capacity)
    ));
    let raw = Connection::open(root.0.join("mail.sqlite3")).unwrap();
    assert_eq!(
        raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    for n in 2..129 {
        store
            .insert(&message(&format!("m{n}"), "a"), State::Pending)
            .unwrap();
    }
    for peek in [false, true] {
        assert!(matches!(
            store.poll_batch("a", "s", 0, peek),
            Err(MailError::Capacity)
        ));
        assert_eq!(store.unread("a").unwrap(), 129);
    }
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
