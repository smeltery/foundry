mod support_witness;
use foundry_store::Store;
use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use support_witness::*;

#[test]
fn witness_and_terminal_write_failures_leave_all_old_state_and_allow_cleanup() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON context_checkpoints BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_checkpoint_heads BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_finalizations BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_checkpoints BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_witnesses BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_witnesses BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_checkpoint_heads BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_finalizations BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail AFTER INSERT ON context_witnesses BEGIN UPDATE attempts SET cancel='hard' WHERE active=1; END",
        "CREATE TRIGGER fail AFTER INSERT ON context_witnesses BEGIN UPDATE attempts SET expires_ms=1 WHERE active=1; END",
        "CREATE TRIGGER fail BEFORE UPDATE OF active ON attempts WHEN NEW.active=0 BEGIN SELECT RAISE(ABORT,'injected'); END",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = checkpoint::session(&mut store);
        let fence = store.begin_attempt(&session, 0).unwrap();
        let before = store.get(&session).unwrap();
        let db = Connection::open(path).unwrap();
        db.execute_batch(trigger).unwrap();
        assert!(
            store
                .finish_with_witnessed_checkpoint(&fence, input(&store, &fence, "w"), terminal())
                .is_err(),
            "{trigger}"
        );
        assert_eq!(store.get(&session).unwrap(), before);
        assert_eq!(store.work_revision(&session).unwrap(), 1);
        for table in [
            "context_checkpoints",
            "context_witnesses",
            "context_checkpoint_heads",
            "context_finalizations",
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        db.execute_batch("DROP TRIGGER fail").unwrap();
        store
            .finish_attempt(&fence, foundry_contracts::Lifecycle::Suspended, None, None)
            .unwrap();
    }
}

#[test]
fn concurrent_witness_publications_have_one_complete_winner_and_exact_readonly_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let left = input(&store, &fence, "same");
    let right = input(&store, &fence, "same");
    let expected = left
        .finalization_request_digest(&fence, &terminal())
        .unwrap();
    let mut other = Store::open(&path).unwrap();
    let other_fence = fence.clone();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = barrier.clone();
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        other
            .finish_with_witnessed_checkpoint(&other_fence, right, terminal())
            .is_ok()
    });
    barrier.wait();
    let won = store
        .finish_with_witnessed_checkpoint(&fence, left, terminal())
        .is_ok();
    assert_ne!(won, worker.join().unwrap());
    assert!(
        store
            .finished_checkpoint_request(&session, "same", expected)
            .unwrap()
            .is_some()
    );
    let db = Connection::open(path).unwrap();
    for table in [
        "context_checkpoints",
        "context_witnesses",
        "context_checkpoint_heads",
        "context_finalizations",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn witnessed_root_cannot_silently_adopt_a_legacy_parent_or_wrong_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let mut value = input(&store, &fence, "bad-header");
    value.witness.header.configuration_digest = [0; 32];
    assert!(
        store
            .finish_with_witnessed_checkpoint(&fence, value, terminal())
            .is_err()
    );
    let mut value = input(&store, &fence, "cannot-adopt");
    let parent = store
        .commit_opaque_checkpoint(&fence, checkpoint::input(&store, &fence, "legacy"))
        .unwrap();
    value.checkpoint.expected_head = Some(parent.id.clone());
    value.witness.header.parent = Some(foundry_store::witness::WitnessParent {
        checkpoint_id: parent.id.to_string(),
        generation: parent.generation,
        record_digest: parent.record_digest,
        witness_digest: [0; 32],
    });
    assert!(
        store
            .finish_with_witnessed_checkpoint(&fence, value, terminal())
            .is_err()
    );
    assert_eq!(store.checkpoint_head(&session).unwrap(), Some(parent));
}
