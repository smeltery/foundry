use super::*;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_runtime::coordinated::CaptureCancelled;

struct Fixture {
    root: tempfile::TempDir,
    path: std::path::PathBuf,
    store: Store,
    session: String,
    owner: FreshCaptureOwner,
    ports: Option<Box<CapturePorts>>,
}
impl Fixture {
    fn new(timeout: Duration) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = create_initial(&mut store);
        let mut owner = FreshCaptureOwner::start(
            path.clone(),
            session.clone(),
            configuration(),
            RestoreOwnerOptions {
                lease: ServiceOptions {
                    lease_duration: Duration::from_secs(2),
                    poll_interval: Duration::from_millis(20),
                },
                cleanup_timeout: timeout,
            },
        )
        .unwrap();
        let FreshCaptureReply::Ready(ports) = receive(&mut owner) else {
            panic!()
        };
        Self {
            root,
            path,
            store,
            session,
            owner,
            ports: Some(ports),
        }
    }
    fn cancelled(&mut self, active: bool) -> Box<CaptureCancelled> {
        let ports = self.ports.take().unwrap();
        let cancellation = self.owner.take_cancellation().unwrap();
        let marker = self.root.path().join("started");
        let mut spec = AdapterSpec::new("/usr/bin/python3");
        spec.args = vec![
            "-u".into(),
            "-c".into(),
            include_str!("cancel_adapter.py").into(),
            marker.clone().into_os_string(),
        ];
        spec.cancel_timeout = Duration::from_millis(50);
        if !active {
            self.owner.cancel(CancelMode::Hard);
        }
        let worker = thread::spawn(move || {
            let (_sender, messages) = mpsc::channel();
            run_fresh_capture(
                &spec,
                &ports.session,
                &ports.attempt_id,
                &cancellation,
                &messages,
                ports.persistence,
                FreshCaptureOptions {
                    profile: ports.profile,
                    owner: ports.owner,
                    policy: None,
                    phase_timeout: Duration::from_secs(2),
                },
            )
        });
        if active {
            let limit = Instant::now() + Duration::from_secs(3);
            while !marker.exists() {
                assert!(Instant::now() < limit);
                thread::sleep(Duration::from_millis(2));
            }
            self.owner.cancel(CancelMode::Graceful);
            self.owner.cancel(CancelMode::Hard);
        }
        let CaptureExit::Cancelled(proof) = worker.join().unwrap().unwrap() else {
            panic!()
        };
        assert_eq!(proof.session_id(), self.session);
        assert_eq!(proof.mode(), CancelMode::Hard);
        assert_eq!(marker.exists(), active);
        proof
    }
}

#[test]
fn cancellation_waits_for_real_cleanup_then_suspends_without_checkpoint() {
    for active in [false, true] {
        let mut h = Fixture::new(Duration::from_secs(3));
        let proof = h.cancelled(active);
        // Delay beyond the initial durable lease to exercise cancellation heartbeats.
        thread::sleep(Duration::from_millis(2100));
        assert!(h.store.get(&h.session).unwrap().active_attempt_id.is_some());
        h.owner.finish_cancelled(*proof).unwrap();
        let FreshCaptureReply::Cancelled(session) = receive(&mut h.owner) else {
            panic!()
        };
        assert_eq!(session.lifecycle, Lifecycle::Suspended);
        assert!(session.active_attempt_id.is_none());
        assert!(session.outcome.is_none());
        assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
        let report = h.owner.shutdown();
        assert!(report.operation.is_ok());
        assert!(report.release.is_ok());
        assert!(!report.expired);
    }
}

#[test]
fn foreign_proof_and_missing_proof_cannot_release_exposed_attempt() {
    let mut one = Fixture::new(Duration::from_secs(2));
    let mut two = Fixture::new(Duration::from_secs(2));
    let proof = one.cancelled(false);
    two.owner.cancel(CancelMode::Hard);
    two.owner.finish_cancelled(*proof).unwrap();
    let limit = Instant::now() + Duration::from_secs(2);
    while two.owner.try_recv().is_ok() {
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(2));
    }
    let report = two.owner.shutdown();
    assert!(report.release.is_err());
    assert!(
        two.store
            .get(&two.session)
            .unwrap()
            .active_attempt_id
            .is_some()
    );
    let report = one.owner.shutdown();
    assert!(report.release.is_err());
    assert!(
        one.store
            .get(&one.session)
            .unwrap()
            .active_attempt_id
            .is_some()
    );
}

#[test]
fn cancelled_cleanup_timeout_is_bounded_and_does_not_invent_proof() {
    let mut h = Fixture::new(Duration::from_millis(60));
    h.owner.cancel(CancelMode::Hard);
    let start = Instant::now();
    while h.owner.try_recv().is_ok() {
        assert!(start.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(2));
    }
    let report = h.owner.shutdown();
    assert!(report.release.is_err());
    assert!(h.store.get(&h.session).unwrap().active_attempt_id.is_some());
    assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
}

#[test]
fn cancelled_finish_storage_failure_never_reports_suspended_success() {
    let mut h = Fixture::new(Duration::from_secs(2));
    let proof = h.cancelled(false);
    let db = rusqlite::Connection::open(&h.path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE OF body ON sessions BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    h.owner.finish_cancelled(*proof).unwrap();
    let limit = Instant::now() + Duration::from_secs(2);
    while let Ok(reply) = h.owner.try_recv() {
        assert!(reply.is_none());
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(2));
    }
    let report = h.owner.shutdown();
    assert!(report.operation.is_err());
    assert!(report.release.is_err());
    assert!(h.store.get(&h.session).unwrap().active_attempt_id.is_some());
    assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
}

#[test]
fn unconsumed_cancellation_reply_does_not_repeat_settlement() {
    let mut h = Fixture::new(Duration::from_secs(2));
    let proof = h.cancelled(false);
    h.owner.finish_cancelled(*proof).unwrap();
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
    assert_eq!(before.lifecycle, Lifecycle::Suspended);
}

#[test]
fn expired_fence_cannot_be_settled_by_a_valid_cleanup_proof() {
    let mut h = Fixture::new(Duration::from_secs(2));
    let proof = h.cancelled(false);
    let db = rusqlite::Connection::open(&h.path).unwrap();
    db.execute("UPDATE attempts SET expires_ms=0", []).unwrap();
    let _ = h.owner.finish_cancelled(*proof);
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Ok(reply) = h.owner.try_recv() {
        assert!(reply.is_none());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let report = h.owner.shutdown();
    assert!(report.operation.is_err());
    assert!(report.release.is_err());
    assert!(h.store.get(&h.session).unwrap().active_attempt_id.is_some());
    assert!(h.store.checkpoint_head(&h.session).unwrap().is_none());
}
