use foundry_contracts::{CancelMode, CreateSession, Lifecycle, Outcome};
use foundry_store::Store;
use serde_json::json;
use std::{
    sync::{Arc, Barrier},
    time::Duration,
};
use tempfile::tempdir;

fn request() -> CreateSession {
    CreateSession {
        prompt: "test".into(),
        directory: "/tmp".into(),
        model: None,
        idempotency_key: Some("key".into()),
    }
}

#[test]
fn restart_preserves_session_idempotency_events_and_outcome() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = store.create(request()).unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let event = store
        .append_event(&fence, "e1", 0, "text", json!("hello"))
        .unwrap();
    store
        .finish_attempt(
            &fence,
            Lifecycle::Exit,
            Some(Outcome::Failure),
            Some("task failed".into()),
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    let restored = store.create(request()).unwrap();
    assert_eq!(restored.id, session.id);
    assert_eq!(restored.outcome, Some(Outcome::Failure));
    assert_eq!(restored.lifecycle, Lifecycle::Exit);
    assert_eq!(store.events(&session.id, 0, 10).unwrap(), vec![event]);
    assert_eq!(store.list(1).unwrap(), vec![restored]);
    let mut other = request();
    other.prompt = "different".into();
    assert!(store.create(other).is_err());
}

#[test]
fn duplicate_events_are_idempotent_and_conflicts_are_rejected() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store.create(request()).unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let first = store
        .append_event(&fence, "e", 1, "text", json!("a"))
        .unwrap();
    assert_eq!(
        first,
        store
            .append_event(&fence, "e", 1, "text", json!("a"))
            .unwrap()
    );
    assert!(
        store
            .append_event(&fence, "e", 1, "text", json!("b"))
            .is_err()
    );
    assert!(
        store
            .append_event(&fence, "e", 0, "text", json!("a"))
            .is_err()
    );
    let second = store
        .append_event(&fence, "e", 2, "text", json!("ab"))
        .unwrap();
    assert_eq!(second.sequence, first.sequence + 1);
    assert_eq!(
        store.events(&session.id, first.sequence, 1).unwrap(),
        vec![second]
    );
    assert!(
        store
            .append_event(&fence, "huge", u64::MAX, "text", json!(null))
            .is_err()
    );
}

#[test]
fn concurrent_create_and_attempt_claim_have_one_winner() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let _store = Store::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                let session = store.create(request()).unwrap();
                let claimed = store.begin_attempt(&session.id, 0).is_ok();
                (session.id, claimed)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results[0].0, results[1].0);
    assert_eq!(results.iter().filter(|x| x.1).count(), 1);
}

#[test]
fn open_and_recovery_do_not_disturb_a_live_lease() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut owner = Store::open(&path).unwrap();
    let session = owner.create(request()).unwrap();
    let fence = owner.begin_attempt(&session.id, 0).unwrap();
    let mut observer = Store::open(&path).unwrap();
    assert!(observer.recover_expired().unwrap().is_empty());
    owner.heartbeat(&fence).unwrap();
    owner
        .append_event(&fence, "e", 0, "text", json!("alive"))
        .unwrap();
    assert!(
        observer
            .begin_attempt(&session.id, observer.get(&session.id).unwrap().version)
            .is_err()
    );
}

#[test]
fn expired_owner_is_fenced_and_recovery_does_not_invent_success() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut owner = Store::open_with_lease(&path, Duration::from_millis(30)).unwrap();
    let session = owner.create(request()).unwrap();
    let stale = owner.begin_attempt(&session.id, 0).unwrap();
    owner
        .append_event(&stale, "tool", 0, "tool_started", json!({"name":"test"}))
        .unwrap();
    drop(owner);
    std::thread::sleep(Duration::from_millis(60));
    let mut replacement = Store::open(&path).unwrap();
    assert_eq!(
        replacement.get(&session.id).unwrap().lifecycle,
        Lifecycle::Running
    );
    let recovered = replacement.recover_expired().unwrap().pop().unwrap();
    assert_eq!(recovered.lifecycle, Lifecycle::Suspended);
    assert_eq!(recovered.outcome, None);
    assert_eq!(recovered.result, None);
    assert!(replacement.recover_expired().unwrap().is_empty());
    let new = replacement
        .begin_attempt(&session.id, recovered.version)
        .unwrap();
    assert!(replacement.heartbeat(&stale).is_err());
    assert!(
        replacement
            .append_event(&stale, "bad", 0, "text", json!(null))
            .is_err()
    );
    assert!(
        replacement
            .finish_attempt(&stale, Lifecycle::Exit, Some(Outcome::Success), None)
            .is_err()
    );
    let next = replacement
        .append_event(&new, "fresh", 0, "text", json!("new"))
        .unwrap();
    assert_eq!(next.sequence, 2);
    assert_eq!(replacement.events(&session.id, 0, 10).unwrap().len(), 2);
}

#[test]
fn cancel_only_upgrades_and_requires_suspended_completion() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store.create(request()).unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    store
        .request_cancel(&session.id, CancelMode::Graceful)
        .unwrap();
    assert_eq!(
        store.cancellation(&fence).unwrap(),
        Some(CancelMode::Graceful)
    );
    store.request_cancel(&session.id, CancelMode::Hard).unwrap();
    let prior = store.get(&session.id).unwrap();
    let next = store
        .request_cancel(&session.id, CancelMode::Graceful)
        .unwrap();
    assert_eq!(prior, next);
    assert_eq!(store.cancellation(&fence).unwrap(), Some(CancelMode::Hard));
    assert!(
        store
            .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
            .is_err()
    );
    let ended = store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    assert!(ended.active_attempt_id.is_none());
    assert!(
        store
            .append_event(&fence, "e", 0, "text", json!(null))
            .is_err()
    );
}

#[test]
fn concurrent_events_are_sequenced_without_lost_writes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = store.create(request()).unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|writer| {
            let path = path.clone();
            let fence = fence.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                for i in 0..10 {
                    store
                        .append_event(&fence, &format!("{writer}-{i}"), 0, "text", json!(i))
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let events = store.events(&session.id, 0, 100).unwrap();
    assert_eq!(events.len(), 20);
    assert_eq!(
        events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        (1..=20).collect::<Vec<_>>()
    );
}

#[test]
fn migrations_are_versioned_and_newer_schemas_are_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    drop(Store::open(&path).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
    drop(Store::open(&path).unwrap());
    connection.pragma_update(None, "user_version", 12).unwrap();
    assert!(Store::open(&path).is_err());
}

#[test]
fn forged_fence_and_stale_version_cannot_write() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store.create(request()).unwrap();
    assert!(store.begin_attempt(&session.id, 9).is_err());
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let mut forged = fence.clone();
    forged.token = "incorrect".into();
    assert!(store.heartbeat(&forged).is_err());
    assert!(
        store
            .append_event(&forged, "e", 0, "text", json!(null))
            .is_err()
    );
    assert!(
        store
            .finish_attempt(&fence, Lifecycle::Exit, None, None)
            .is_err()
    );
    assert!(
        store
            .finish_attempt(&fence, Lifecycle::Running, None, None)
            .is_err()
    );
    assert_eq!(
        store.get(&session.id).unwrap().lifecycle,
        Lifecycle::Running
    );
}
