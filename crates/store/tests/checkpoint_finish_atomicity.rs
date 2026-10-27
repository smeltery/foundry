mod support_checkpoint;
use foundry_contracts::{CancelMode, Lifecycle, Outcome};
use foundry_store::{Store, TerminalProposal, ToolOutcome};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use support_checkpoint::{input, session};
use tempfile::tempdir;

fn terminal() -> TerminalProposal {
    TerminalProposal {
        lifecycle: Lifecycle::Exit,
        outcome: Some(Outcome::Success),
        result: None,
    }
}

#[test]
fn failure_at_each_write_rolls_back_blob_head_terminal_receipt_and_deactivation() {
    for (stage, trigger) in [
        (
            "blob",
            "BEFORE INSERT ON context_checkpoints BEGIN SELECT RAISE(ABORT,'injected'); END",
        ),
        (
            "head",
            "BEFORE UPDATE ON context_checkpoint_heads BEGIN SELECT RAISE(ABORT,'injected'); END",
        ),
        (
            "terminal",
            "BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'injected'); END",
        ),
        (
            "receipt",
            "BEFORE INSERT ON context_finalizations BEGIN SELECT RAISE(ABORT,'injected'); END",
        ),
        (
            "deactivate",
            "BEFORE UPDATE OF active ON attempts BEGIN SELECT RAISE(ABORT,'injected'); END",
        ),
        (
            "late-expiry",
            "AFTER INSERT ON context_finalizations BEGIN UPDATE attempts SET expires_ms=0; END",
        ),
        (
            "late-cancel",
            "AFTER INSERT ON context_finalizations BEGIN UPDATE attempts SET cancel='hard'; END",
        ),
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let prior_head = store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "prior"))
            .unwrap();
        let before = store.get(&id).unwrap();
        let sql = Connection::open(&path).unwrap();
        sql.execute_batch(&format!("CREATE TRIGGER failure {trigger};"))
            .unwrap();
        let mut value = input(&store, &fence, "atomic");
        value.expected_head = Some(prior_head.id.clone());
        let digest = value
            .finalization_request_digest(&fence, &terminal())
            .unwrap();
        assert!(
            store
                .finish_with_opaque_checkpoint(&fence, value, terminal())
                .is_err(),
            "{stage}"
        );
        assert_eq!(store.get(&id).unwrap(), before, "{stage}");
        assert_eq!(
            store.checkpoint_head(&id).unwrap(),
            Some(prior_head),
            "{stage}"
        );
        assert!(
            store.checkpoint_request(&id, "atomic").unwrap().is_none(),
            "{stage}"
        );
        assert!(
            store
                .finished_checkpoint_request(&id, "atomic", digest)
                .unwrap()
                .is_none(),
            "{stage}"
        );
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM context_checkpoints", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "{stage}"
        );
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM context_finalizations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{stage}"
        );
        assert_eq!(
            sql.query_row("SELECT active FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1,
            "{stage}"
        );
        assert_eq!(store.cancellation(&fence).unwrap(), None, "{stage}");
        store.heartbeat(&fence).unwrap();
        sql.execute_batch("DROP TRIGGER failure;").unwrap();
        // A rejected checkpoint must not strand ordinary failure cleanup.
        store
            .finish_attempt(&fence, Lifecycle::Error, None, None)
            .unwrap();
    }
}

#[test]
fn cancelled_unresolved_and_inflight_work_keep_ordinary_cleanup_available() {
    for case in 0..3 {
        let mut store = Store::open(":memory:").unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let value = input(&store, &fence, "blocked");
        match case {
            0 => {
                store.request_cancel(&id, CancelMode::Graceful).unwrap();
            }
            1 => {
                store.claim_tool(&fence, "tool", "write", [1; 32]).unwrap();
                store
                    .finish_tool(&fence, "tool", ToolOutcome::Uncertain, None)
                    .unwrap();
            }
            _ => {
                store.enqueue_message(&id, "message", "pending").unwrap();
                store.claim_next_message(&fence).unwrap();
            }
        }
        let before = store.get(&id).unwrap();
        assert!(
            store
                .finish_with_opaque_checkpoint(&fence, value, terminal())
                .is_err()
        );
        assert_eq!(store.get(&id).unwrap(), before);
        assert!(store.checkpoint_head(&id).unwrap().is_none());
        store
            .finish_attempt(&fence, Lifecycle::Suspended, None, None)
            .unwrap();
        assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Suspended);
    }
}

#[test]
fn competing_finalizations_have_one_complete_winner() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|n| {
            let path = path.clone();
            let fence = fence.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                let value = input(&store, &fence, &format!("request-{n}"));
                barrier.wait();
                store.finish_with_opaque_checkpoint(&fence, value, terminal())
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(winner.checkpoint));
    assert_eq!(store.get(&id).unwrap().version, winner.session_version);
    assert!(store.get(&id).unwrap().active_attempt_id.is_none());
    let sql = Connection::open(path).unwrap();
    for table in ["context_checkpoints", "context_finalizations"] {
        assert_eq!(
            sql.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn cancellation_race_is_serialized_with_atomic_publication() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let value = input(&store, &fence, "race");
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_id = id.clone();
    let mut canceller = Store::open(&path).unwrap();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        canceller
            .request_cancel(&worker_id, CancelMode::Hard)
            .unwrap();
    });
    barrier.wait();
    let finished = store.finish_with_opaque_checkpoint(&fence, value, terminal());
    worker.join().unwrap();
    match finished {
        Ok(receipt) => {
            assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Exit);
            assert_eq!(
                store.checkpoint_head(&id).unwrap(),
                Some(receipt.checkpoint)
            );
        }
        Err(_) => {
            assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Running);
            assert!(store.checkpoint_head(&id).unwrap().is_none());
            assert_eq!(store.cancellation(&fence).unwrap(), Some(CancelMode::Hard));
            store
                .finish_attempt(&fence, Lifecycle::Suspended, None, None)
                .unwrap();
        }
    }
}
