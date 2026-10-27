mod support_restore;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_store::{BeginRestoreResult, ContinueDecision, RestoreClose, RestorePhase, Store};
use rusqlite::Connection;
use support_restore::{begin, capture, continuation, request};
use tempfile::tempdir;

#[test]
fn load_confirm_close_preserve_exact_prior_body_and_admit_no_work() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let before = store.get(&id).unwrap();
    let revision = store.work_revision(&id).unwrap();
    let sql = Connection::open(&path).unwrap();
    let body: String = sql
        .query_row("SELECT body FROM sessions WHERE id=?1", [&id], |r| r.get(0))
        .unwrap();
    let fence = begin(&mut store, &id, &checkpoint, "load");
    assert!(store.begin_attempt(&id, before.version).is_err());
    assert!(
        store
            .authorize_continue(&fence, continuation("continue"))
            .is_err()
    );
    let confirmed = store.confirm_restored(&fence, [7; 32]).unwrap();
    assert_eq!(confirmed.phase, RestorePhase::Restored);
    assert_eq!(store.confirm_restored(&fence, [7; 32]).unwrap(), confirmed);
    store.heartbeat_restore(&fence).unwrap();
    assert_eq!(store.work_revision(&id).unwrap(), revision);
    assert_eq!(store.get(&id).unwrap(), before);
    let closed = store.close_restore(&fence, RestoreClose::Closed).unwrap();
    assert_eq!(closed.phase, RestorePhase::Closed);
    let after: String = sql
        .query_row("SELECT body FROM sessions WHERE id=?1", [&id], |r| r.get(0))
        .unwrap();
    assert_eq!(after, body);
    assert!(store.heartbeat_restore(&fence).is_err());
    store.begin_attempt(&id, before.version).unwrap();
}

#[test]
fn begin_retries_are_metadata_only_and_changed_selection_fails() {
    let mut store = Store::open(":memory:").unwrap();
    let (id, checkpoint) = capture(&mut store);
    let fence = begin(&mut store, &id, &checkpoint, "load");
    let retried = store
        .begin_restore(&id, request(&store, &id, &checkpoint, "load"))
        .unwrap();
    assert!(
        matches!(retried,BeginRestoreResult::Existing(reference) if reference.id==fence.restore_id())
    );
    let mut wrong = request(&store, &id, &checkpoint, "load");
    wrong.configuration_digest = [8; 32];
    assert!(store.begin_restore(&id, wrong).is_err());
    assert!(store.confirm_restored(&fence, [8; 32]).is_err());
    assert!(
        store
            .begin_restore(&id, request(&store, &id, &checkpoint, "competing"))
            .is_err()
    );
    store.close_restore(&fence, RestoreClose::Failed).unwrap();
    let mut wrong = request(&store, &id, &checkpoint, "new");
    wrong.configuration_digest = [8; 32];
    assert!(store.begin_restore(&id, wrong).is_err());
    assert!(store.restore_request(&id, "new").unwrap().is_none());
    let ordinary = store
        .begin_attempt(&id, store.get(&id).unwrap().version)
        .unwrap();
    store
        .finish_attempt(&ordinary, Lifecycle::Suspended, None, None)
        .unwrap();
    // Even an eventless intervening attempt invalidates the checkpoint's work cut.
    assert!(
        store
            .begin_restore(&id, request(&store, &id, &checkpoint, "stale"))
            .is_err()
    );
}

#[test]
fn continue_is_once_only_and_historical_receipt_survives_reopen_and_later_attempts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let fence = begin(&mut store, &id, &checkpoint, "load");
    store.confirm_restored(&fence, [7; 32]).unwrap();
    let revision = store.work_revision(&id).unwrap();
    let (attempt, receipt) = match store
        .authorize_continue(&fence, continuation("go"))
        .unwrap()
    {
        ContinueDecision::Granted { permit, receipt } => (permit.into_attempt_fence(), receipt),
        _ => panic!("first authorization must grant one permit"),
    };
    assert_eq!(store.work_revision(&id).unwrap(), revision + 1);
    assert_eq!(receipt.admitted_work_revision, revision + 1);
    assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Running);
    assert!(store.get(&id).unwrap().result.is_none());
    assert!(store.close_restore(&fence, RestoreClose::Closed).is_err());
    assert!(store.heartbeat_restore(&fence).is_err());
    assert!(
        store
            .authorize_continue(&fence, continuation("other"))
            .is_err()
    );
    let mut wrong = continuation("go");
    wrong.delivery_digest = [0; 32];
    assert!(store.authorize_continue(&fence, wrong).is_err());
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(
        matches!(store.authorize_continue(&fence,continuation("go")).unwrap(),ContinueDecision::AlreadyAuthorized(ref found) if found==&receipt)
    );
    store
        .finish_attempt(
            &attempt,
            Lifecycle::Error,
            None,
            Some("uncertain execution".into()),
        )
        .unwrap();
    assert_eq!(
        store.restore_request(&id, "load").unwrap().unwrap().phase,
        RestorePhase::Finished
    );
    store
        .begin_attempt(&id, store.get(&id).unwrap().version)
        .unwrap();
    let before = store.get(&id).unwrap();
    assert!(
        matches!(store.authorize_continue(&fence,continuation("go")).unwrap(),ContinueDecision::AlreadyAuthorized(ref found) if found==&receipt)
    );
    assert_eq!(store.get(&id).unwrap(), before);
}

#[test]
fn cancellation_retains_loading_owner_until_cleanup_without_overwriting_outcome() {
    let mut store = Store::open(":memory:").unwrap();
    let (id, checkpoint) = capture(&mut store);
    let before = store.get(&id).unwrap();
    let fence = begin(&mut store, &id, &checkpoint, "load");
    store.request_cancel(&id, CancelMode::Hard).unwrap();
    store.request_cancel(&id, CancelMode::Graceful).unwrap();
    assert_eq!(
        store.restore_cancellation(&fence).unwrap(),
        Some(CancelMode::Hard)
    );
    assert_eq!(store.get(&id).unwrap(), before);
    assert!(store.begin_attempt(&id, before.version).is_err());
    assert!(store.confirm_restored(&fence, [7; 32]).is_err());
    assert!(
        store
            .authorize_continue(&fence, continuation("go"))
            .is_err()
    );
    store.heartbeat_restore(&fence).unwrap();
    assert_eq!(
        store
            .close_restore(&fence, RestoreClose::Closed)
            .unwrap()
            .phase,
        RestorePhase::Cancelled
    );
    assert_eq!(store.get(&id).unwrap(), before);
    begin(&mut store, &id, &checkpoint, "another");
}

#[test]
fn expiry_preserves_loading_body_but_continuing_expiry_suspends_uncertain_execution() {
    for phase in 0..3 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (id, checkpoint) = capture(&mut store);
        let before = store.get(&id).unwrap();
        let fence = begin(&mut store, &id, &checkpoint, "load");
        if phase > 0 {
            store.confirm_restored(&fence, [7; 32]).unwrap();
        }
        if phase == 2 {
            store
                .authorize_continue(&fence, continuation("go"))
                .unwrap();
        }
        let sql = Connection::open(&path).unwrap();
        sql.execute("UPDATE context_restores SET expires_ms=1", [])
            .unwrap();
        if phase == 2 {
            assert!(store.recover_expired_restores().unwrap().is_empty());
            sql.execute("UPDATE attempts SET expires_ms=0 WHERE active=1", [])
                .unwrap();
            store.recover_expired().unwrap();
            assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Suspended);
            assert!(matches!(
                store
                    .authorize_continue(&fence, continuation("go"))
                    .unwrap(),
                ContinueDecision::AlreadyAuthorized(_)
            ));
        } else {
            assert!(store.begin_attempt(&id, before.version).is_err());
            assert!(store.confirm_restored(&fence, [7; 32]).is_err());
            assert!(
                store
                    .authorize_continue(&fence, continuation("go"))
                    .is_err()
            );
            store.recover_expired_restores().unwrap();
            assert_eq!(store.get(&id).unwrap(), before);
            assert_eq!(
                store.work_revision(&id).unwrap(),
                checkpoint.cut.work_revision
            );
        }
        assert_eq!(
            store.restore_request(&id, "load").unwrap().unwrap().phase,
            RestorePhase::Expired
        );
    }
}

#[test]
fn migration_seven_adds_no_restore_authority() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let before = store.get(&id).unwrap();
    drop(store);
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; PRAGMA user_version=7;")
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.get(&id).unwrap(), before);
    assert!(store.restore_request(&id, "load").unwrap().is_none());
    begin(&mut store, &id, &checkpoint, "load");
}
