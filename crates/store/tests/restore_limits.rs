mod support_restore;
use foundry_contracts::Lifecycle;
use foundry_store::{
    BeginRestoreResult, ContinueDecision, MAX_RESTORE_ROWS_PER_DATABASE,
    MAX_RESTORE_ROWS_PER_SESSION, RESTORE_RECOVERY_BATCH_SIZE, RestoreClose, Store,
};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use support_restore::{begin, capture, continuation, request};
use tempfile::tempdir;

#[test]
fn session_cap_preserves_history_and_allows_exact_retry_cleanup_and_continuation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let before = store.get(&id).unwrap();
    for n in 0..MAX_RESTORE_ROWS_PER_SESSION - 1 {
        let fence = begin(&mut store, &id, &checkpoint, &format!("history-{n}"));
        store.close_restore(&fence, RestoreClose::Closed).unwrap();
    }
    let fence = begin(&mut store, &id, &checkpoint, "last");
    assert!(matches!(
        store
            .begin_restore(&id, request(&store, &id, &checkpoint, "history-0"))
            .unwrap(),
        BeginRestoreResult::Existing(_)
    ));
    let error = store
        .begin_restore(&id, request(&store, &id, &checkpoint, "over"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("quota"));
    assert_eq!(store.get(&id).unwrap(), before);
    assert_eq!(
        store.work_revision(&id).unwrap(),
        checkpoint.cut.work_revision
    );
    assert!(store.restore_request(&id, "over").unwrap().is_none());
    store.confirm_restored(&fence, [7; 32]).unwrap();
    let attempt = match store
        .authorize_continue(&fence, continuation("go"))
        .unwrap()
    {
        ContinueDecision::Granted { permit, .. } => permit.into_attempt_fence(),
        _ => panic!("first authorization must grant"),
    };
    store
        .finish_attempt(&attempt, Lifecycle::Suspended, None, None)
        .unwrap();
    let sql = Connection::open(path).unwrap();
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM context_restores", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        MAX_RESTORE_ROWS_PER_SESSION
    );
}

#[test]
fn database_cap_counts_all_retained_rows_and_retry_still_works() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (source, source_checkpoint) = capture(&mut store);
    let fence = begin(&mut store, &source, &source_checkpoint, "source");
    store.close_restore(&fence, RestoreClose::Closed).unwrap();
    let (target, checkpoint) = capture(&mut store);
    let fence = begin(&mut store, &target, &checkpoint, "target");
    store.close_restore(&fence, RestoreClose::Closed).unwrap();
    let sql = Connection::open(&path).unwrap();
    // Synthetic legacy rows exercise accounting, not restore authority. Every retained
    // row must count even if its digest would fail decoding or its session exceeds a cap.
    sql.execute(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<?1)
         INSERT INTO context_restores(id,session_id,checkpoint_id,checkpoint_request_digest,
             configuration_digest,request_id,begin_digest,expected_session_version,token,expires_ms,phase)
         SELECT printf('%08x-0000-0000-0000-%012x',x,x),session_id,checkpoint_id,checkpoint_request_digest,
             configuration_digest,'legacy-'||x,begin_digest,expected_session_version,
             printf('%08x-1111-0000-0000-%012x',x,x),expires_ms,'closed'
         FROM n CROSS JOIN context_restores WHERE request_id='source'",
        [MAX_RESTORE_ROWS_PER_DATABASE-2],
    ).unwrap();
    let before = store.get(&target).unwrap();
    assert!(matches!(
        store
            .begin_restore(&target, request(&store, &target, &checkpoint, "target"))
            .unwrap(),
        BeginRestoreResult::Existing(_)
    ));
    let error = store
        .begin_restore(&target, request(&store, &target, &checkpoint, "over"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("quota"));
    assert_eq!(store.get(&target).unwrap(), before);
    assert!(store.restore_request(&target, "over").unwrap().is_none());
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM context_restores", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        MAX_RESTORE_ROWS_PER_DATABASE
    );
}

#[test]
fn concurrent_last_slot_admission_never_exceeds_retained_cap() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    for n in 0..MAX_RESTORE_ROWS_PER_SESSION - 1 {
        let fence = begin(&mut store, &id, &checkpoint, &format!("history-{n}"));
        store.close_restore(&fence, RestoreClose::Closed).unwrap();
    }
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|n| {
            let mut worker = Store::open(&path).unwrap();
            let id = id.clone();
            let value = request(&worker, &id, &checkpoint, &format!("competitor-{n}"));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match worker.begin_restore(&id, value) {
                    Ok(BeginRestoreResult::Started { fence, .. }) => {
                        worker.close_restore(&fence, RestoreClose::Closed).unwrap();
                        Ok(())
                    }
                    Err(error) => Err(error),
                    _ => panic!("distinct new request cannot be a retry"),
                }
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .into_iter()
            .find_map(Result::err)
            .unwrap()
            .to_string()
            .contains("quota")
    );
    let sql = Connection::open(path).unwrap();
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM context_restores", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        MAX_RESTORE_ROWS_PER_SESSION
    );
}

#[test]
fn recovery_is_ordered_batched_atomic_and_preserves_prior_bodies() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    for n in 0..RESTORE_RECOVERY_BATCH_SIZE + 1 {
        let (id, checkpoint) = capture(&mut store);
        let fence = begin(&mut store, &id, &checkpoint, "load");
        if n % 2 == 0 {
            store.confirm_restored(&fence, [7; 32]).unwrap();
        }
    }
    let sql = Connection::open(&path).unwrap();
    sql.execute(
        "UPDATE context_restores SET expires_ms=CASE WHEN substr(id,1,1)<'8' THEN 1 ELSE 2 END",
        [],
    )
    .unwrap();
    let bodies = || {
        sql.prepare("SELECT body FROM sessions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = bodies();
    let ids: Vec<String> = sql
        .prepare("SELECT id FROM context_restores ORDER BY expires_ms,id COLLATE BINARY")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    sql.execute_batch(&format!("CREATE TRIGGER fail_batch BEFORE UPDATE OF phase ON context_restores WHEN OLD.id='{}' BEGIN SELECT RAISE(ABORT,'injected'); END;",ids[100])).unwrap();
    assert!(store.recover_expired_restores().is_err());
    assert_eq!(
        sql.query_row(
            "SELECT COUNT(*) FROM context_restores WHERE phase='expired'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(bodies(), before);
    sql.execute_batch("DROP TRIGGER fail_batch;").unwrap();
    let first = store.recover_expired_restores().unwrap();
    assert_eq!(first.len(), RESTORE_RECOVERY_BATCH_SIZE as usize);
    assert_eq!(
        first.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[..128]
    );
    let second = store.recover_expired_restores().unwrap();
    assert_eq!(
        second.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[128..]
    );
    assert!(store.recover_expired_restores().unwrap().is_empty());
    assert_eq!(bodies(), before);
}
