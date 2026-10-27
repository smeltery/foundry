use foundry_contracts::{CancelMode, CreateSession, Lifecycle, MessageState};
use foundry_store::Store;
use rusqlite::Connection;

fn create(store: &mut Store) -> String {
    store
        .create(CreateSession {
            prompt: "prompt\n\\\0🦀".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
        .id
}

#[test]
fn fresh_returns_exact_session_allows_unclaimed_queue_and_is_consumed_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = create(&mut store);
    let long_id = "x".repeat(129);
    store.enqueue_message(&id, &long_id, "first").unwrap();
    store
        .enqueue_message(&id, "control\0id", "withdraw")
        .unwrap();
    store.withdraw_message(&id, "control\0id").unwrap();
    let queue = store.messages(&id, 0, 10).unwrap();
    // Neither counter is a freshness heuristic: actual history absence is decisive.
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE sessions SET body=json_set(body,'$.version',7)", [])
        .unwrap();
    db.execute("UPDATE session_work SET revision=11", [])
        .unwrap();
    assert!(store.begin_fresh_attempt(&id, 0).is_err());
    let fresh = store.begin_fresh_attempt(&id, 7).unwrap();
    assert_eq!(fresh.session().prompt, "prompt\n\\\0🦀");
    assert_eq!(fresh.session().version, 8);
    assert_eq!(fresh.session().lifecycle, Lifecycle::Running);
    assert_eq!(
        fresh.session().active_attempt_id.as_deref(),
        Some(fresh.fence().attempt_id.as_str())
    );
    assert_eq!(store.work_revision(&id).unwrap(), 12);
    assert_eq!(store.messages(&id, 0, 10).unwrap(), queue);
    let (fence, session) = fresh.into_parts();
    assert_eq!(session, store.get(&id).unwrap());
    // This stands for a subsequent trusted activation, not automatic admission-time delivery.
    assert_eq!(
        store.claim_next_message(&fence).unwrap().unwrap().id,
        long_id
    );
    drop(store);
    let mut reopened = Store::open(path).unwrap();
    assert!(reopened.begin_fresh_attempt(&id, session.version).is_err());
    assert_eq!(
        reopened.messages(&id, 0, 10).unwrap()[0].state,
        MessageState::InFlight
    );
}

#[test]
fn eventless_prior_admission_never_becomes_fresh_after_cleanup_or_lost_reply() {
    for recovery in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        let fresh = store.begin_fresh_attempt(&id, 0).unwrap();
        let db = Connection::open(&path).unwrap();
        if recovery {
            db.execute("UPDATE attempts SET expires_ms=1", []).unwrap();
            store.recover_expired().unwrap();
        } else {
            store
                .finish_attempt(fresh.fence(), Lifecycle::Suspended, None, None)
                .unwrap();
        }
        drop(fresh);
        drop(store);
        // Simulate migration-like counters/body; retained eventless attempt remains decisive.
        db.execute("UPDATE session_work SET revision=0", [])
            .unwrap();
        db.execute(
            "UPDATE sessions SET body=json_set(body,'$.lifecycle','queued')",
            [],
        )
        .unwrap();
        let mut store = Store::open(&path).unwrap();
        let before = store.get(&id).unwrap();
        assert!(store.events(&id, 0, 10).unwrap().is_empty());
        assert!(store.checkpoint_head(&id).unwrap().is_none());
        assert!(store.begin_fresh_attempt(&id, before.version).is_err());
        assert_eq!(store.get(&id).unwrap(), before);
        // Ordinary admission semantics are deliberately unchanged.
        assert!(store.begin_attempt(&id, before.version).is_ok());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = create(&mut store);
    drop(store.begin_fresh_attempt(&id, 0).unwrap()); // response discarded before Start
    drop(store);
    let mut store = Store::open(path).unwrap();
    assert!(store.begin_fresh_attempt(&id, 1).is_err());
    assert!(store.events(&id, 0, 1).unwrap().is_empty());
}

#[test]
fn queued_cancellation_and_immutable_creation_changes_reject_without_admission() {
    for mutation in [
        "UPDATE sessions SET body=json_set(body,'$.prompt','different')",
        "UPDATE sessions SET body=json_set(body,'$.directory','/elsewhere')",
        "UPDATE sessions SET body=json_set(body,'$.model',json('{}'))",
        "UPDATE sessions SET body=json_set(body,'$.id','different')",
        "UPDATE sessions SET body=json_set(body,'$.result','old-result')",
        "UPDATE sessions SET creation='invalid-json'",
        "DELETE FROM session_work",
        "PRAGMA ignore_check_constraints=ON; UPDATE session_work SET revision=-1",
        "PRAGMA ignore_check_constraints=ON; UPDATE session_work SET revision=0.5",
        "UPDATE session_work SET revision=9223372036854775807",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        let db = Connection::open(path).unwrap();
        db.execute_batch(mutation).unwrap();
        let before: String = db
            .query_row("SELECT body FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert!(store.begin_fresh_attempt(&id, 0).is_err(), "{mutation}");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT body FROM sessions", [], |r| r.get::<_, String>(0))
                .unwrap(),
            before
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("db")).unwrap();
    let id = create(&mut store);
    store.request_cancel(&id, CancelMode::Hard).unwrap();
    let before = store.get(&id).unwrap();
    assert!(store.begin_fresh_attempt(&id, before.version).is_err());
    assert_eq!(store.get(&id).unwrap(), before);
    assert!(store.begin_fresh_attempt("absent", 0).is_err());
}
