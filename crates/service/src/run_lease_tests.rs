use super::*;
use foundry_contracts::CreateSession;
use std::{sync::atomic::AtomicU8, time::Duration};

#[test]
fn frozen_claims_keep_the_real_store_lease_alive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease.db");
    let options = ServiceOptions {
        lease_duration: Duration::from_secs(2),
        poll_interval: Duration::from_millis(20),
    };
    let mut store = Store::open_with_lease(&path, options.lease_duration).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "retain context".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
    let (cancel, cancelled) = mpsc::channel();
    let (messages, _received) = mpsc::sync_channel(1);
    let lease = RunLease::start(
        Store::open_with_lease(&path, options.lease_duration).unwrap(),
        &fence,
        options,
        Instant::now() + options.lease_duration * 2 / 3,
        signals.clone(),
        cancel,
        messages,
    )
    .unwrap();
    lease.freeze_claims();
    // Cross the original durable lease deadline; only real heartbeats keep it valid.
    thread::sleep(options.lease_duration + Duration::from_millis(200));
    assert_eq!(store.cancellation(&fence).unwrap(), None);
    assert!(!lease.expired().load(Ordering::Acquire));
    assert!(cancelled.try_recv().is_err());
    signals.accepting.store(true, Ordering::Release);
    assert!(signals.admission.try_claim().is_none());
    let report = lease.join();
    assert!(!report.expired);
    assert!(!report.watchdog_panicked);
    report.monitor_result.unwrap();
}

#[test]
fn join_reports_watchdog_panic_and_still_joins_monitor() {
    let (stop, stopped) = mpsc::channel();
    let (renew, _renewed) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let lease = RunLease {
        settlement: Arc::new(Mutex::new(())),
        signals: RunControl::new(Arc::new(AtomicU8::new(0))),
        expired: Arc::new(AtomicBool::new(false)),
        stop,
        renew,
        watchdog: Some(thread::spawn(|| panic!("injected watchdog failure"))),
        monitor: Some(thread::spawn(move || {
            stopped.recv().unwrap();
            finished.send(()).unwrap();
            Err(ServiceError::Coordination("injected monitor failure"))
        })),
    };
    let report = lease.join();
    assert!(report.watchdog_panicked);
    assert!(matches!(
        report.monitor_result,
        Err(ServiceError::Coordination("injected monitor failure"))
    ));
    completion.try_recv().unwrap();
}

#[test]
fn runtime_receiver_closure_does_not_end_cancelled_owner_renewal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled-lease.db");
    let options = ServiceOptions {
        lease_duration: Duration::from_secs(2),
        poll_interval: Duration::from_millis(20),
    };
    let mut store = Store::open_with_lease(&path, options.lease_duration).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "settle after cleanup".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    store
        .request_cancel(&session.id, CancelMode::Graceful)
        .unwrap();
    let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
    let (cancel, cancelled) = mpsc::channel();
    drop(cancelled);
    let (messages, received) = mpsc::sync_channel(1);
    drop(received);
    let lease = RunLease::start(
        Store::open_with_lease(&path, options.lease_duration).unwrap(),
        &fence,
        options,
        Instant::now() + options.lease_duration * 2 / 3,
        signals.clone(),
        cancel,
        messages,
    )
    .unwrap();
    lease.freeze_claims();
    assert!(
        lease
            .settle(|| Ok(store.finish_attempt(
                &fence,
                foundry_contracts::Lifecycle::Exit,
                Some(foundry_contracts::Outcome::Success),
                Some("completed".into()),
            )?))
            .is_err()
    );
    thread::sleep(options.lease_duration + Duration::from_millis(200));
    assert_eq!(
        store.cancellation(&fence).unwrap(),
        Some(CancelMode::Graceful)
    );
    assert!(!lease.expired().load(Ordering::Acquire));
    assert!(signals.admission.try_claim().is_none());
    store.request_cancel(&session.id, CancelMode::Hard).unwrap();
    assert_eq!(store.cancellation(&fence).unwrap(), Some(CancelMode::Hard));
    let settled =
        lease
            .settle(|| {
                Ok(store.finish_attempt(
                    &fence,
                    foundry_contracts::Lifecycle::Suspended,
                    None,
                    None,
                )?)
            })
            .unwrap();
    assert!(settled.active_attempt_id.is_none());
    let report = lease.join();
    assert!(!report.expired && !report.watchdog_panicked);
    report.monitor_result.unwrap();
}

#[test]
fn partial_owner_drop_stops_and_joins_watchdog() {
    let (stop, _stopped) = mpsc::channel();
    let (renew, renewed) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
    let lease = RunLease {
        settlement: Arc::new(Mutex::new(())),
        signals: signals.clone(),
        expired: Arc::new(AtomicBool::new(false)),
        stop,
        renew,
        watchdog: Some(thread::spawn(move || {
            assert_eq!(renewed.recv().unwrap(), None);
            finished.send(()).unwrap();
        })),
        monitor: None,
    };
    drop(lease);
    completion.try_recv().unwrap();
    assert!(signals.admission.try_claim().is_none());
}
