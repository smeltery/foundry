mod support_checkpoint;
use foundry_contracts::{Lifecycle, Outcome};
use foundry_store::{Store, TerminalProposal};
use rusqlite::Connection;
use support_checkpoint::{input, session};
use tempfile::tempdir;

fn terminal() -> TerminalProposal {
    TerminalProposal {
        lifecycle: Lifecycle::Exit,
        outcome: Some(Outcome::Success),
        result: Some("historical result".into()),
    }
}

#[test]
fn atomic_finish_recovers_exact_history_after_reopen_and_later_attempts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let value = input(&store, &fence, "finish");
    let stale_retry = input(&store, &fence, "finish");
    let digest = value
        .finalization_request_digest(&fence, &terminal())
        .unwrap();
    assert_ne!(digest, value.request_digest(&fence).unwrap());
    let result = store
        .finish_with_opaque_checkpoint(&fence, value, terminal())
        .unwrap();
    assert_eq!(result.checkpoint.request_digest, digest);
    let stored_record: [u8; 32] = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT record_digest FROM context_checkpoints WHERE id=?1",
            [result.checkpoint.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result.checkpoint.record_digest, stored_record);
    let finished = store.get(&id).unwrap();
    assert_eq!(finished.lifecycle, Lifecycle::Exit);
    assert_eq!(finished.result, result.terminal.result);
    assert_eq!(finished.version, result.session_version);
    assert!(finished.active_attempt_id.is_none());
    assert!(store.heartbeat(&fence).is_err());
    assert!(
        store
            .finish_with_opaque_checkpoint(&fence, stale_retry, terminal())
            .is_err()
    );
    assert_eq!(store.get(&id).unwrap(), finished);
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .finished_checkpoint_request(&id, "finish", digest)
            .unwrap(),
        Some(result.clone())
    );
    let next = store.begin_attempt(&id, finished.version).unwrap();
    let mut changed = terminal();
    changed.result = Some("later result".into());
    let mut value = input(&store, &next, "later");
    value.expected_head = Some(result.checkpoint.id.clone());
    let later = store
        .finish_with_opaque_checkpoint(&next, value, changed)
        .unwrap();
    let current = store.get(&id).unwrap();
    assert_ne!(current.version, result.session_version);
    assert_ne!(current.result, result.terminal.result);
    assert_eq!(
        store
            .finished_checkpoint_request(&id, "finish", digest)
            .unwrap(),
        Some(result.clone())
    );
    assert_eq!(store.get(&id).unwrap(), current);
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(later.checkpoint));
    assert!(
        store
            .finished_checkpoint_request(&id, "finish", [0; 32])
            .is_err()
    );
    assert_eq!(
        store
            .finished_checkpoint_request(&id, "absent", digest)
            .unwrap(),
        None
    );
    assert!(
        store
            .read_opaque_checkpoint(&id, &result.checkpoint.id)
            .is_ok()
    );
}

#[test]
fn invalid_terminal_and_changed_proposal_have_distinct_validation_and_identity() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let expected = input(&store, &fence, "a")
        .finalization_request_digest(&fence, &terminal())
        .unwrap();
    let mut changed = terminal();
    changed.result = Some("different".into());
    assert_ne!(
        input(&store, &fence, "a")
            .finalization_request_digest(&fence, &changed)
            .unwrap(),
        expected
    );
    for (lifecycle, outcome, result) in [
        (Lifecycle::Exit, None, None),
        (Lifecycle::Running, None, None),
        (Lifecycle::Error, Some(Outcome::Failure), None),
        (Lifecycle::Suspended, None, Some("x".repeat(16384))),
    ] {
        let proposal = TerminalProposal {
            lifecycle,
            outcome,
            result,
        };
        assert!(
            store
                .finish_with_opaque_checkpoint(&fence, input(&store, &fence, "a"), proposal)
                .is_err()
        );
        assert!(store.checkpoint_head(&id).unwrap().is_none());
        assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Running);
    }
}

#[test]
fn migration_six_preserves_plain_checkpoints_without_inventing_finalization() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let reference = store
        .commit_opaque_checkpoint(&fence, input(&store, &fence, "plain"))
        .unwrap();
    drop(store);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; DROP TABLE context_finalizations; PRAGMA user_version=6;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(reference.clone()));
    assert!(
        store
            .finished_checkpoint_request(&id, "plain", reference.request_digest)
            .is_err()
    );
    let mut value = input(&store, &fence, "finish");
    value.expected_head = Some(reference.id);
    store
        .finish_with_opaque_checkpoint(&fence, value, terminal())
        .unwrap();
}

#[test]
fn corrupted_or_missing_finalization_receipts_are_not_acknowledged() {
    for mutation in [
        "UPDATE context_finalizations SET session_version=session_version+1",
        "UPDATE context_finalizations SET receipt_digest=zeroblob(32)",
        "DELETE FROM context_finalizations",
        "UPDATE context_checkpoints SET record_digest=zeroblob(32)",
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let result = store
            .finish_with_opaque_checkpoint(&fence, input(&store, &fence, "a"), terminal())
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(mutation)
            .unwrap();
        assert!(
            store
                .finished_checkpoint_request(&id, "a", result.checkpoint.request_digest)
                .is_err()
        );
    }
}
