#![cfg(unix)]
use foundry_network::mail_store::{
    DeliveryIntent, Insert, MAX_DELIVERY_ATTEMPTS, MailError, MailStore, Message, Settlement, State,
};
use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("mail-test-{:032x}", u128::from_le_bytes(nonce)));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn open(&self) -> MailStore {
        MailStore::open(&self.0).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn message(id: &str) -> Message {
    Message {
        id: id.into(),
        sender: "alice".into(),
        recipient: "bob".into(),
        body: b"hello".to_vec(),
        encoding: foundry_network::mail_store::PayloadEncoding::Text,
        reply_to: None,
        intent: DeliveryIntent::default(),
    }
}
#[test]
fn immutable_ids_and_retained_outbox() {
    let root = Root::new();
    let mut db = root.open();
    let mail = message("one");
    assert_eq!(db.insert(&mail, State::Pending).unwrap(), Insert::Created);
    assert_eq!(db.insert(&mail, State::Held).unwrap(), Insert::Duplicate);
    for changed in [
        Message {
            sender: "other".into(),
            ..mail.clone()
        },
        Message {
            recipient: "other".into(),
            ..mail.clone()
        },
        Message {
            body: b"different".to_vec(),
            ..mail.clone()
        },
    ] {
        assert!(matches!(
            db.insert(&changed, State::Pending),
            Err(MailError::Conflict)
        ));
    }
    let delivery = db.claim("bob", "s1", 10, 100).unwrap().unwrap();
    assert_eq!(db.unread("bob").unwrap(), 1);
    assert_eq!(db.peek("bob", 10).unwrap(), vec![mail.clone()]);
    assert!(matches!(
        db.acknowledge(&delivery.attempt, "s2", 11),
        Err(MailError::StaleAttempt)
    ));
    db.acknowledge(&delivery.attempt, "s1", 11).unwrap();
    assert!(matches!(
        db.acknowledge(&delivery.attempt, "s1", 12),
        Err(MailError::StaleAttempt)
    ));
    assert_eq!(db.unread("bob").unwrap(), 0);
    assert_eq!(db.insert(&mail, State::Pending).unwrap(), Insert::Duplicate);
    assert_eq!(db.outbox("alice", 10).unwrap()[0].state, State::Delivered);
    drop(db);
    assert_eq!(root.open().get("one").unwrap().state, State::Delivered);
}
#[test]
fn expiration_reopen_failure_and_disconnect_recover() {
    let root = Root::new();
    let mut db = root.open();
    db.insert(&message("one"), State::Pending).unwrap();
    let old = db.claim("bob", "s1", 10, 10).unwrap().unwrap();
    drop(db);
    let mut db = root.open();
    assert!(db.claim("bob", "s2", 19, 10).unwrap().is_none());
    assert!(matches!(
        db.acknowledge(&old.attempt, "s1", 20),
        Err(MailError::StaleAttempt)
    ));
    assert!(db.claim("bob", "s2", 1019, 10).unwrap().is_none());
    let new = db.claim("bob", "s2", 1020, 10).unwrap().unwrap();
    assert_ne!(old.attempt.id(), new.attempt.id());
    assert!(matches!(
        db.fail(&old.attempt, "s1", 1021),
        Err(MailError::StaleAttempt)
    ));
    db.fail(&new.attempt, "s2", 1021).unwrap();
    let third = db.claim("bob", "s3", 3021, 10).unwrap().unwrap();
    assert_eq!(db.disconnect("s2").unwrap(), 0);
    assert_eq!(db.disconnect("s3").unwrap(), 1);
    assert!(matches!(
        db.acknowledge(&third.attempt, "s3", 3022),
        Err(MailError::StaleAttempt)
    ));
    drop(db);
    assert_eq!(root.open().unread("bob").unwrap(), 1);
}
#[test]
fn held_and_rejected_are_distinct() {
    let root = Root::new();
    let mut db = root.open();
    db.insert(&message("held"), State::Held).unwrap();
    db.insert(&message("no"), State::Held).unwrap();
    assert!(db.claim("bob", "s", 0, 10).unwrap().is_none());
    db.settle("no", Settlement::Reject).unwrap();
    db.settle("held", Settlement::Release).unwrap();
    assert!(matches!(
        db.settle("no", Settlement::Release),
        Err(MailError::InvalidTransition)
    ));
    assert_eq!(db.get("no").unwrap().state, State::Rejected);
    assert_eq!(
        db.claim("bob", "s", 0, 10).unwrap().unwrap().mail.id,
        "held"
    );
}
#[test]
fn concurrent_handles_cannot_claim_same_row() {
    let root = Root::new();
    root.open().insert(&message("one"), State::Pending).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let path = root.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut db = MailStore::open(&path).unwrap();
                barrier.wait();
                db.claim("bob", &format!("s{i}"), 0, 100).unwrap().is_some()
            })
        })
        .collect();
    let claims = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(claims, 1);
}
#[test]
fn bounds_and_storage_protection() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = Root::new();
    let mut db = root.open();
    assert!(matches!(
        db.insert(
            &Message {
                body: vec![0; 262145],
                ..message("big")
            },
            State::Pending
        ),
        Err(MailError::InvalidInput)
    ));
    assert!(db.peek("bob", 101).is_err());
    assert!(db.claim("bob", "s", i64::MAX, 1).is_err());
    drop(db);
    std::fs::set_permissions(
        root.0.join("mail.sqlite3"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(MailStore::open(&root.0).is_err());
    let other = Root::new();
    symlink(root.0.join("mail.sqlite3"), other.0.join("mail.sqlite3")).unwrap();
    assert!(MailStore::open(&other.0).is_err());
}

#[test]
fn concurrent_exact_insert_and_schema_failure() {
    let root = Root::new();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = root.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut db = MailStore::open(&path).unwrap();
                barrier.wait();
                db.insert(&message("same"), State::Pending).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| **r == Insert::Created).count(), 1);
    assert_eq!(
        results.iter().filter(|r| **r == Insert::Duplicate).count(),
        1
    );
    assert_eq!(root.open().unread("bob").unwrap(), 1);
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    raw.execute_batch("PRAGMA user_version=4").unwrap();
    drop(raw);
    assert!(matches!(MailStore::open(&root.0), Err(MailError::Schema)));
}
#[test]
fn empty_and_held_mail_survive_reopen() {
    let root = Root::new();
    let mut db = root.open();
    let empty = Message {
        body: vec![],
        ..message("empty")
    };
    db.insert(&empty, State::Pending).unwrap();
    db.insert(&message("held"), State::Held).unwrap();
    drop(db);
    let db = root.open();
    assert_eq!(db.peek("bob", 10).unwrap(), vec![empty]);
    assert_eq!(db.get("held").unwrap().state, State::Held);
    assert_eq!(db.outbox("alice", 10).unwrap().len(), 2);
}

#[path = "mail_store/payload_tests.rs"]
mod payload_tests;
#[path = "mail_store/polling_tests.rs"]
mod polling_tests;
#[path = "mail_store/retry_tests.rs"]
mod retry_tests;
