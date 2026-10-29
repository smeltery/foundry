use super::*;
use std::time::Duration;

#[test]
fn elapsed_deadline_and_sticky_cancel_cannot_be_revived() {
    let requested = Arc::new(AtomicU8::new(0));
    let gate = PermissionGate::new(requested.clone());
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Uninitialized));
    let old = Instant::now() - Duration::from_millis(1);
    gate.initialize(old);
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Expired));
    assert!(!gate.renew(old, Instant::now() + Duration::from_secs(2)));
    gate.initialize(Instant::now() + Duration::from_secs(10));
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Revoked));
    let gate = PermissionGate::new(requested.clone());
    gate.initialize(Instant::now() + Duration::from_secs(10));
    requested.store(1, Ordering::Release);
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Revoked));
    requested.store(0, Ordering::Release);
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Revoked));
}

#[test]
fn generation_and_revocation_serialize_with_consumption() {
    let gate = PermissionGate::new(Arc::new(AtomicU8::new(0)));
    gate.initialize(Instant::now() + Duration::from_secs(10));
    let generation = gate.generation().unwrap();
    gate.invalidate();
    assert_eq!(gate.consume(generation, || ()), Err(GateFailure::Stale));
    let current = gate.generation().unwrap();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let consumer = gate.clone();
    let worker = std::thread::spawn(move || {
        consumer.consume(current, || {
            entered.send(()).unwrap();
            released.recv().unwrap();
        })
    });
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    let revoker = gate.clone();
    let stop = std::thread::spawn(move || revoker.revoke());
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    stop.join().unwrap();
    assert_eq!(gate.consume(current, || ()), Err(GateFailure::Revoked));
}

#[test]
fn sqlite_wait_cannot_hold_permission_past_the_lease_deadline() {
    use crate::{ServiceOptions, control::RunControl, run_lease::RunLease};
    use foundry_contracts::{CancelMode, CreateSession};
    use foundry_store::Store;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("permission.db");
    let options = ServiceOptions {
        lease_duration: Duration::from_secs(2),
        poll_interval: Duration::from_millis(20),
    };
    let mut store = Store::open_with_lease(&path, options.lease_duration).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "inspect".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let monitor = Store::open_with_lease(&path, options.lease_duration).unwrap();
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
    let (cancel, cancelled) = std::sync::mpsc::channel();
    let (messages, _received) = std::sync::mpsc::sync_channel(1);
    let lease = RunLease::start(
        monitor,
        &fence,
        options,
        Instant::now() + Duration::from_millis(150),
        signals.clone(),
        cancel,
        messages,
    )
    .unwrap();
    assert_eq!(
        cancelled.recv_timeout(Duration::from_secs(2)).unwrap(),
        CancelMode::Hard
    );
    assert_eq!(
        signals.permissions.consume(0, || ()),
        Err(GateFailure::Revoked)
    );
    lock.execute_batch("ROLLBACK").unwrap();
    let report = lease.join();
    assert!(report.expired);
    assert!(!report.watchdog_panicked);
    assert_eq!(
        signals.permissions.consume(0, || ()),
        Err(GateFailure::Revoked)
    );
}

#[test]
fn renewal_waiting_on_consumption_cannot_revive_an_elapsed_lease() {
    let gate = PermissionGate::new(Arc::new(AtomicU8::new(0)));
    let deadline = Instant::now() + Duration::from_millis(80);
    gate.initialize(deadline);
    let held = gate.state.lock().unwrap();
    let renewing = gate.clone();
    let (started, waiting) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        renewing.renew(deadline, deadline + Duration::from_secs(1))
    });
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    std::thread::sleep(
        deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
    );
    drop(held);
    assert!(!worker.join().unwrap());
    assert_eq!(gate.consume(0, || ()), Err(GateFailure::Revoked));
}
