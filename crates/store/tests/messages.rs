use foundry_contracts::{CreateSession, Lifecycle, MessageState, Outcome};
use foundry_store::Store;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

fn create(store: &mut Store) -> foundry_contracts::Session {
    store
        .create(CreateSession {
            prompt: "fixture".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
}

#[test]
fn idempotency_pagination_and_withdrawal_survive_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let first = store
        .enqueue_message(&session.id, "one", "continue")
        .unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(
        store
            .enqueue_message(&session.id, "one", "continue")
            .unwrap(),
        first
    );
    assert!(
        store
            .enqueue_message(&session.id, "one", "different")
            .is_err()
    );
    let second = store.enqueue_message(&session.id, "two", "next").unwrap();
    assert_eq!(second.sequence, 2);
    let withdrawn = store.withdraw_message(&session.id, "one").unwrap();
    assert_eq!(withdrawn.state, MessageState::Withdrawn);
    drop(store);
    let mut store = Store::open(path).unwrap();
    assert_eq!(
        store
            .enqueue_message(&session.id, "one", "continue")
            .unwrap(),
        withdrawn
    );
    assert_eq!(store.messages(&session.id, 1, 1).unwrap(), vec![second]);
    assert_eq!(store.get(&session.id).unwrap().lifecycle, Lifecycle::Queued);
    assert!(store.messages(&session.id, u64::MAX, 1).is_err());
}

#[test]
fn delivery_is_fenced_and_does_not_imply_execution() {
    let mut store = Store::open(":memory:").unwrap();
    let session = create(&mut store);
    store
        .enqueue_message(&session.id, "one", "continue")
        .unwrap();
    store.enqueue_message(&session.id, "two", "other").unwrap();
    store.withdraw_message(&session.id, "two").unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let mut forged = fence.clone();
    forged.token = "invalid".into();
    assert!(store.acknowledge_message(&forged, "one").is_err());
    assert!(store.acknowledge_message(&fence, "two").is_err());
    assert!(store.acknowledge_message(&fence, "one").is_err());
    let claimed = store.claim_next_message(&fence).unwrap().unwrap();
    assert_eq!(claimed.id, "one");
    assert_eq!(claimed.state, MessageState::InFlight);
    assert!(store.withdraw_message(&session.id, "one").is_err());
    assert!(store.claim_next_message(&fence).unwrap().is_none());
    let delivered = store.acknowledge_message(&fence, "one").unwrap();
    assert_eq!(delivered.state, MessageState::Delivered);
    assert_eq!(store.acknowledge_message(&fence, "one").unwrap(), delivered);
    assert!(store.withdraw_message(&session.id, "one").is_err());
    let running = store.get(&session.id).unwrap();
    assert_eq!(running.lifecycle, Lifecycle::Running);
    assert_eq!(running.outcome, None);
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    assert!(store.acknowledge_message(&fence, "one").is_err());
}

#[test]
fn concurrent_retries_have_one_sequence() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let id = session.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store.enqueue_message(&id, "same", "instruction").unwrap()
            })
        })
        .collect();
    let messages: Vec<_> = threads
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(messages[0], messages[1]);
    assert_eq!(store.messages(&session.id, 0, 100).unwrap().len(), 1);
}

#[test]
fn version_one_migration_preserves_sessions_and_adds_queue() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; DROP TABLE context_finalizations; DROP TABLE context_checkpoint_heads; DROP TABLE context_checkpoints; DROP TABLE session_work; DROP TABLE tool_calls; DROP TABLE messages; PRAGMA user_version=1;")
        .unwrap();
    drop(connection);
    let mut store = Store::open(path).unwrap();
    assert_eq!(store.get(&session.id).unwrap(), session);
    assert!(
        store
            .enqueue_message(&session.id, "new", "instruction")
            .is_ok()
    );
    assert!(store.enqueue_message(&session.id, "blank", " ").is_err());
    assert!(
        store
            .enqueue_message("missing", "new", "instruction")
            .is_err()
    );
}

#[test]
fn uncertain_delivery_survives_recovery_without_requeue() {
    use std::time::Duration;
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = Store::open_with_lease(&path, Duration::from_millis(20)).unwrap();
    let session = create(&mut store);
    store.enqueue_message(&session.id, "one", "first").unwrap();
    store.enqueue_message(&session.id, "two", "second").unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let claimed = store.claim_next_message(&fence).unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(40));
    store.recover_expired().unwrap();
    assert!(store.acknowledge_message(&fence, "one").is_err());
    drop(store);
    let mut store = Store::open(path).unwrap();
    let recovered = store.get(&session.id).unwrap();
    let next = store.begin_attempt(&session.id, recovered.version).unwrap();
    assert_eq!(store.messages(&session.id, 0, 1).unwrap(), vec![claimed]);
    assert!(store.claim_next_message(&next).unwrap().is_none());
    assert!(store.acknowledge_message(&next, "one").is_err());
    assert!(store.withdraw_message(&session.id, "one").is_err());
}

#[test]
fn claim_and_withdraw_cannot_both_win() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    store.enqueue_message(&session.id, "one", "first").unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        let mut store = Store::open(worker_path).unwrap();
        worker_barrier.wait();
        store.claim_next_message(&fence).unwrap()
    });
    barrier.wait();
    let withdrawn = store.withdraw_message(&session.id, "one");
    let claimed = worker.join().unwrap();
    assert_ne!(claimed.is_some(), withdrawn.is_ok());
    let state = store.messages(&session.id, 0, 1).unwrap()[0].state;
    assert!(matches!(
        state,
        MessageState::InFlight | MessageState::Withdrawn
    ));
}

#[test]
fn cancellation_prevents_claim_without_stranding_pending_message() {
    use foundry_contracts::CancelMode;
    for mode in [CancelMode::Graceful, CancelMode::Hard] {
        let mut store = Store::open(":memory:").unwrap();
        let session = create(&mut store);
        store
            .enqueue_message(&session.id, "one", "instruction")
            .unwrap();
        let fence = store.begin_attempt(&session.id, session.version).unwrap();
        store.request_cancel(&session.id, mode).unwrap();
        assert!(store.claim_next_message(&fence).unwrap().is_none());
        assert_eq!(
            store.messages(&session.id, 0, 1).unwrap()[0].state,
            MessageState::Pending
        );
        assert_eq!(
            store.withdraw_message(&session.id, "one").unwrap().state,
            MessageState::Withdrawn
        );
    }
}
