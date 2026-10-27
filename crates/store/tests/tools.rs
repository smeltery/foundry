use foundry_contracts::{CancelMode, CreateSession, Lifecycle, Outcome};
use foundry_store::{Store, ToolOutcome};
use std::time::Duration;
use tempfile::tempdir;

fn create(store: &mut Store) -> foundry_contracts::Session {
    store
        .create(CreateSession {
            prompt: "tool fixture".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
}

#[test]
fn claims_and_completions_survive_restart_without_repeat_dispatch() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let claim = store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
    assert_eq!(claim.outcome, None);
    assert!(store.claim_tool(&fence, "call", "write", [1; 32]).is_err());
    assert!(store.claim_tool(&fence, "next", "read", [2; 32]).is_err());
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.tool_call(&session.id, "call").unwrap(), Some(claim));
    assert!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Success, None)
            .is_err()
    );
    let complete = store
        .finish_tool(&fence, "call", ToolOutcome::Success, Some([3; 32]))
        .unwrap();
    assert_eq!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Success, Some([3; 32]))
            .unwrap(),
        complete
    );
    assert!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Success, Some([4; 32]))
            .is_err()
    );
    assert!(store.claim_tool(&fence, "call", "write", [1; 32]).is_err());
    assert!(store.claim_tool(&fence, "next", "read", [2; 32]).is_ok());
}

#[test]
fn cancellation_prevents_dispatch_and_success_receipts() {
    for mode in [CancelMode::Graceful, CancelMode::Hard] {
        let mut store = Store::open(":memory:").unwrap();
        let session = create(&mut store);
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
        store.request_cancel(&session.id, mode).unwrap();
        assert!(store.claim_tool(&fence, "new", "read", [2; 32]).is_err());
        assert!(
            store
                .finish_tool(&fence, "call", ToolOutcome::Success, Some([3; 32]))
                .is_err()
        );
        assert_eq!(
            store
                .tool_call(&session.id, "call")
                .unwrap()
                .unwrap()
                .outcome,
            None
        );
    }
}

#[test]
fn crashed_claim_survives_recovery_and_blocks_new_attempt_dispatch() {
    let mut store = Store::open_with_lease(":memory:", Duration::from_millis(50)).unwrap();
    let session = create(&mut store);
    let old = store.begin_attempt(&session.id, 0).unwrap();
    store.claim_tool(&old, "call", "write", [1; 32]).unwrap();
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        store
            .finish_tool(&old, "call", ToolOutcome::Success, Some([3; 32]))
            .is_err()
    );
    let recovered = store.recover_expired().unwrap().remove(0);
    assert_eq!(recovered.lifecycle, Lifecycle::Suspended);
    let fresh = store.begin_attempt(&session.id, recovered.version).unwrap();
    assert!(
        store
            .finish_tool(&fresh, "call", ToolOutcome::Failed, None)
            .is_err()
    );
    assert!(store.claim_tool(&fresh, "call", "write", [1; 32]).is_err());
    assert!(store.claim_tool(&fresh, "new", "read", [2; 32]).is_err());
    assert_eq!(
        store
            .tool_call(&session.id, "call")
            .unwrap()
            .unwrap()
            .attempt_id,
        old.attempt_id
    );
}

#[test]
fn uncertain_effects_block_dispatch_but_confirmed_no_effect_failure_does_not() {
    for (outcome, permits_next) in [(ToolOutcome::Uncertain, false), (ToolOutcome::Failed, true)] {
        let mut store = Store::open(":memory:").unwrap();
        let session = create(&mut store);
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
        store.finish_tool(&fence, "call", outcome, None).unwrap();
        assert_eq!(
            store.claim_tool(&fence, "next", "read", [2; 32]).is_ok(),
            permits_next
        );
    }
}

#[test]
fn migration_from_three_preserves_existing_session_and_messages() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let message = store
        .enqueue_message(&session.id, "message", "continue")
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE context_continue_acceptances; DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; DROP TABLE context_finalizations; DROP TABLE context_checkpoint_heads; DROP TABLE context_checkpoints; DROP TABLE session_work; DROP TABLE tool_calls; PRAGMA user_version=3;")
        .unwrap();
    drop(connection);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.get(&session.id).unwrap(), session);
    assert_eq!(store.messages(&session.id, 0, 10).unwrap(), vec![message]);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    assert!(store.claim_tool(&fence, "call", "read", [1; 32]).is_ok());
}

#[test]
fn forged_fence_unknown_call_and_invalid_identifiers_do_not_write() {
    let mut store = Store::open(":memory:").unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let mut forged = fence.clone();
    forged.token.push('x');
    assert!(store.claim_tool(&forged, "call", "read", [1; 32]).is_err());
    assert!(
        store
            .finish_tool(&fence, "missing", ToolOutcome::Failed, None)
            .is_err()
    );
    for id in ["".to_string(), " ".into(), "x\ny".into(), "x".repeat(129)] {
        assert!(store.claim_tool(&fence, &id, "read", [1; 32]).is_err());
    }
    assert_eq!(store.tool_call(&session.id, "call").unwrap(), None);
}

#[test]
fn unresolved_calls_prevent_terminal_outcomes_across_attempts() {
    for outcome in [None, Some(ToolOutcome::Uncertain)] {
        let mut store = Store::open(":memory:").unwrap();
        let session = create(&mut store);
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
        if let Some(outcome) = outcome {
            store.finish_tool(&fence, "call", outcome, None).unwrap();
        }
        for result in [Outcome::Success, Outcome::Failure, Outcome::ActionRequired] {
            assert!(
                store
                    .finish_attempt(&fence, Lifecycle::Exit, Some(result), None)
                    .is_err()
            );
        }
        let suspended = store
            .finish_attempt(&fence, Lifecycle::Suspended, None, None)
            .unwrap();
        let next = store.begin_attempt(&session.id, suspended.version).unwrap();
        assert!(
            store
                .finish_attempt(&next, Lifecycle::Exit, Some(Outcome::Success), None)
                .is_err()
        );
        assert!(
            store
                .finish_attempt(&next, Lifecycle::Error, None, None)
                .is_ok()
        );
    }
}

#[test]
fn concurrent_callers_receive_only_one_dispatch_permit() {
    use std::sync::{Arc, Barrier};
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| {
            let path = path.clone();
            let fence = fence.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store.claim_tool(&fence, id, "write", [1; 32]).is_ok()
            })
        })
        .collect();
    let permits = threads
        .into_iter()
        .map(|thread| usize::from(thread.join().unwrap()))
        .sum::<usize>();
    assert_eq!(permits, 1);
    let persisted = ["one", "two"]
        .into_iter()
        .filter(|id| store.tool_call(&session.id, id).unwrap().is_some())
        .count();
    assert_eq!(persisted, 1);
}
