mod support_checkpoint;
use foundry_contracts::CancelMode;
use foundry_store::{Store, ToolOutcome};
use rusqlite::Connection;
use serde_json::json;
use std::sync::{Arc, Barrier};
use support_checkpoint::{input, session};
use tempfile::tempdir;

#[test]
fn changed_work_inflight_inputs_and_unresolved_tools_cannot_publish() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let stale = input(&store, &fence, "stale");
    store
        .append_event(&fence, "e", 0, "text", json!("work"))
        .unwrap();
    assert!(store.commit_opaque_checkpoint(&fence, stale).is_err());
    let mut value = input(&store, &fence, "covered");
    let prior = value.cut.clone();
    store.enqueue_message(&id, "message", "input").unwrap();
    assert_eq!(store.checkpoint_cut(&fence).unwrap(), prior);
    store.claim_next_message(&fence).unwrap().unwrap();
    assert!(store.checkpoint_cut(&fence).is_err());
    store.acknowledge_message(&fence, "message").unwrap();
    assert!(store.commit_opaque_checkpoint(&fence, value).is_err());
    value = input(&store, &fence, "covered");
    assert_ne!(value.cut.input_digest, prior.input_digest);
    assert!(store.commit_opaque_checkpoint(&fence, value).is_err());
    store.claim_tool(&fence, "tool", "read", [1; 32]).unwrap();
    assert!(store.checkpoint_cut(&fence).is_err());
    store
        .finish_tool(&fence, "tool", ToolOutcome::Success, Some([2; 32]))
        .unwrap();
    let mut value = input(&store, &fence, "covered");
    assert_ne!(value.cut.tool_digest, prior.tool_digest);
    value.coverage.message_ids = vec!["message".into()];
    value.coverage.tool_call_ids = vec!["tool".into()];
    let reference = store.commit_opaque_checkpoint(&fence, value).unwrap();
    assert_eq!(reference.cut.event_seq, 1);
    let coverage = store.read_opaque_checkpoint(&id, &reference.id).unwrap();
    assert_eq!(coverage.coverage().message_ids, ["message"]);
    store
        .claim_tool(&fence, "uncertain", "write", [3; 32])
        .unwrap();
    store
        .finish_tool(&fence, "uncertain", ToolOutcome::Uncertain, None)
        .unwrap();
    assert!(store.checkpoint_cut(&fence).is_err());
}

#[test]
fn head_failure_rolls_back_blob_and_request_then_retry_succeeds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let sql = Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_head BEFORE INSERT ON context_checkpoint_heads BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "retry"))
            .is_err()
    );
    assert!(store.checkpoint_head(&id).unwrap().is_none());
    assert!(store.checkpoint_request(&id, "retry").unwrap().is_none());
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM context_checkpoints", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    sql.execute_batch("DROP TRIGGER fail_head;").unwrap();
    store
        .commit_opaque_checkpoint(&fence, input(&store, &fence, "retry"))
        .unwrap();
}

#[test]
fn concurrent_publish_cas_has_one_winner_and_same_request_retries_converge() {
    for same_request in [false, true] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|n| {
                let path = path.clone();
                let fence = fence.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open(&path).unwrap();
                    let request = if same_request {
                        "same".into()
                    } else {
                        format!("request-{n}")
                    };
                    let value = input(&store, &fence, &request);
                    barrier.wait();
                    store.commit_opaque_checkpoint(&fence, value)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            if same_request { 2 } else { 1 }
        );
        if same_request {
            assert_eq!(results[0].as_ref().unwrap(), results[1].as_ref().unwrap());
        }
        let sql = Connection::open(path).unwrap();
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM context_checkpoints", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(store.checkpoint_head(&id).unwrap().unwrap().generation, 1);
    }
}

#[test]
fn cancellation_foreign_fence_and_expiry_reject_without_mutation() {
    for case in 0..3 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let other = session(&mut store);
        let mut fence = store.begin_attempt(&id, 0).unwrap();
        let value = input(&store, &fence, "blocked");
        match case {
            0 => {
                store.request_cancel(&id, CancelMode::Graceful).unwrap();
            }
            1 => fence.session_id = other.clone(),
            _ => {
                Connection::open(&path)
                    .unwrap()
                    .execute("UPDATE attempts SET expires_ms=0", [])
                    .unwrap();
            }
        }
        assert!(store.checkpoint_cut(&fence).is_err());
        assert!(store.commit_opaque_checkpoint(&fence, value).is_err());
        assert!(store.checkpoint_head(&id).unwrap().is_none());
        assert!(store.checkpoint_head(&other).unwrap().is_none());
    }
}

#[test]
fn owner_is_checked_after_writes_before_commit() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let sql = Connection::open(&path).unwrap();
    // Deterministic stand-in for the lease expiring during publication.
    sql.execute_batch("CREATE TRIGGER expire_during_write AFTER INSERT ON context_checkpoints BEGIN UPDATE attempts SET expires_ms=0; END;").unwrap();
    assert!(
        store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "late"))
            .is_err()
    );
    assert!(store.checkpoint_head(&id).unwrap().is_none());
    assert!(store.checkpoint_request(&id, "late").unwrap().is_none());
    assert!(store.checkpoint_cut(&fence).is_ok()); // Trigger mutation rolled back too.
}

#[test]
fn cut_revision_and_event_cursor_share_a_read_snapshot_during_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut writer = Store::open(&path).unwrap();
    let id = session(&mut writer);
    let fence = writer.begin_attempt(&id, 0).unwrap();
    let reader = Store::open(&path).unwrap();
    let writer_fence = fence.clone();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writing = std::thread::spawn(move || {
        writer_barrier.wait();
        for n in 0..100 {
            writer
                .append_event(&writer_fence, &format!("event-{n}"), 0, "text", json!(n))
                .unwrap();
        }
    });
    barrier.wait();
    for _ in 0..100 {
        let cut = reader.checkpoint_cut(&fence).unwrap();
        assert_eq!(cut.work_revision, cut.event_seq + 1);
    }
    writing.join().unwrap();
    assert_eq!(reader.checkpoint_cut(&fence).unwrap().event_seq, 100);
}
