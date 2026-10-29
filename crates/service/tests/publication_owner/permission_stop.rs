use super::*;
use foundry_contracts::{Lifecycle, Outcome};
use foundry_runtime::coordinated::CaptureActionRequired;

struct Pending {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    store: Store,
    session: String,
    owner: FreshCaptureOwner,
    stop: Box<CaptureActionRequired>,
    revocation: foundry_service::permission::ToolPermissionRevocation,
}

fn pending() -> Pending {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = create_initial(&mut store);
    let (policy, permissions) = permissions::selection(directory.path(), false);
    let revocation = permissions.revocation();
    let mut publication = configuration();
    publication.profile.configuration_digest = permissions.configuration_digest();
    let mut owner = FreshCaptureOwner::start_with_permissions(
        path.clone(),
        session.clone(),
        publication,
        RestoreOwnerOptions {
            lease: ServiceOptions {
                lease_duration: Duration::from_secs(4),
                poll_interval: Duration::from_millis(25),
            },
            cleanup_timeout: Duration::from_millis(100),
        },
        permissions,
    )
    .unwrap();
    let FreshCaptureReply::Ready(ports) = receive(&mut owner) else {
        panic!()
    };
    let cancellation = owner.take_cancellation().unwrap();
    let (_sender, messages) = mpsc::channel();
    let mut spec = AdapterSpec::new("/usr/bin/python3");
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("permission_adapter.py").into(),
    ];
    spec.run_timeout = Duration::from_secs(4);
    let result = run_fresh_capture(
        &spec,
        &ports.session,
        &ports.attempt_id,
        &cancellation,
        &messages,
        ports.persistence,
        FreshCaptureOptions {
            profile: ports.profile,
            owner: ports.owner,
            policy: Some(policy),
            phase_timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    let CaptureExit::ActionRequired(stop) = result else {
        panic!("permission stop required")
    };
    assert_eq!(stop.session_id(), session);
    assert_eq!(stop.attempt_id(), ports.attempt_id);
    assert!(store.tool_call(&session, "read-once").unwrap().is_none());
    assert!(store.checkpoint_head(&session).unwrap().is_none());
    assert!(
        store
            .events(&session, 0, 100)
            .unwrap()
            .iter()
            .all(|event| event.kind != "settled")
    );
    Pending {
        _directory: directory,
        path,
        store,
        session,
        owner,
        stop,
        revocation,
    }
}

#[test]
fn pending_capture_finishes_action_required_without_publication() {
    let mut h = pending();
    h.owner.finish_action_required(*h.stop).unwrap();
    let FreshCaptureReply::ActionRequired(session) = receive(&mut h.owner) else {
        panic!()
    };
    assert_eq!(session.lifecycle, Lifecycle::Exit);
    assert_eq!(session.outcome, Some(Outcome::ActionRequired));
    assert_eq!(
        session.result.as_deref(),
        Some("permission_action_required")
    );
    assert!(session.active_attempt_id.is_none());
    assert_eq!(*session, h.store.get(&h.session).unwrap());
    assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
    let report = h.owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();
}

#[test]
fn discarded_cleanup_proof_retains_exposed_attempt() {
    let h = pending();
    drop(h.stop);
    assert!(h.owner.shutdown().release.is_err());
    assert!(h.store.get(&h.session).unwrap().active_attempt_id.is_some());
}

#[test]
fn another_owner_cannot_consume_cleanup_proof() {
    let h = pending();
    let other = tempfile::tempdir().unwrap();
    let path = other.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = create_initial(&mut store);
    let mut owner = start_for_test(path, session.clone(), configuration());
    let FreshCaptureReply::Ready(_ports) = receive(&mut owner) else {
        panic!()
    };
    owner.finish_action_required(*h.stop).unwrap();
    expect_initial_error(&mut owner);
    assert!(owner.shutdown().release.is_err());
    assert!(store.get(&session).unwrap().active_attempt_id.is_some());
    assert!(h.owner.shutdown().release.is_err());
}

#[test]
fn revoked_permission_after_cleanup_cannot_finish_action_required() {
    let mut h = pending();
    h.revocation.invalidate();
    h.owner.finish_action_required(*h.stop).unwrap();
    expect_initial_error(&mut h.owner);
    let report = h.owner.shutdown();
    assert!(report.operation.is_err());
    report.release.unwrap();
    let session = h.store.get(&h.session).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Error);
    assert!(session.outcome.is_none());
    assert!(session.active_attempt_id.is_none());
}

#[test]
fn cancellation_after_cleanup_never_records_action_required() {
    let mut h = pending();
    h.store
        .request_cancel(&h.session, foundry_contracts::CancelMode::Hard)
        .unwrap();
    h.owner.finish_action_required(*h.stop).unwrap();
    let FreshCaptureReply::Cancelled(result) = receive(&mut h.owner) else {
        panic!("expected suspended cancellation")
    };
    assert_eq!(result.lifecycle, Lifecycle::Suspended);
    assert!(result.outcome.is_none());
    let report = h.owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();
    let session = h.store.get(&h.session).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Suspended);
    assert!(session.outcome.is_none());
    assert!(session.active_attempt_id.is_none());
    assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
}

#[test]
fn failed_store_finish_never_reports_success_or_releases_without_commit() {
    let mut h = pending();
    let database = rusqlite::Connection::open(&h.path).unwrap();
    database.execute_batch("CREATE TRIGGER prevent_stop BEFORE UPDATE ON attempts BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    h.owner.finish_action_required(*h.stop).unwrap();
    expect_initial_error(&mut h.owner);
    let report = h.owner.shutdown();
    assert!(report.operation.is_err());
    assert!(report.release.is_err());
    let session = h.store.get(&h.session).unwrap();
    assert!(session.active_attempt_id.is_some());
    assert!(session.outcome.is_none());
}

#[test]
fn unconsumed_reply_does_not_repeat_durable_finish() {
    let mut h = pending();
    h.owner.finish_action_required(*h.stop).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while h.store.get(&h.session).unwrap().active_attempt_id.is_some() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let before = h.store.get(&h.session).unwrap();
    let report = h.owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();
    assert_eq!(before, h.store.get(&h.session).unwrap());
    assert_eq!(before.outcome, Some(Outcome::ActionRequired));
}
