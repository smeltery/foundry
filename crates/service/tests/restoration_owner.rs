mod restoration_owner_support;
use foundry_contracts::{CancelMode, context::ContextReceipt};
use foundry_service::restoration_owner::*;
use foundry_store::{RestorePhase, Store};
use restoration_owner_support::*;
use rusqlite::Connection;
use std::{
    thread,
    time::{Duration, Instant},
};

#[test]
fn real_loading_lease_survives_initial_expiry_then_closes_without_admission() {
    let fixture = Fixture::new(Duration::from_secs(2));
    let before = Store::open(&fixture.path)
        .unwrap()
        .get(&fixture.session)
        .unwrap();
    let mut owner = fixture.start("load");
    let _scope = fixture.reserved(&mut owner);
    thread::sleep(Duration::from_millis(2250));
    assert!(!owner.expired());
    let store = Store::open(&fixture.path).unwrap();
    assert_eq!(
        store
            .restore_request(&fixture.session, "load")
            .unwrap()
            .unwrap()
            .phase,
        RestorePhase::Loading
    );
    let report = owner.shutdown();
    report.operation.unwrap();
    assert_eq!(report.release.unwrap().unwrap().phase, RestorePhase::Closed);
    assert!(!report.expired && !report.watchdog_panicked);
    assert_eq!(store.get(&fixture.session).unwrap(), before);
    assert!(store.events(&fixture.session, 0, 100).unwrap().is_empty());
}

#[test]
fn selected_baseline_is_bounded_correlated_once_and_not_native_cleanup() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    fixture.reserved(&mut owner);
    let job = owner.request_baseline().unwrap();
    assert!(owner.request_baseline().is_err());
    let reply = receive(&mut owner);
    assert!(reply.job_id == job);
    let RestoreReply::Baseline(snapshot) = reply.result.unwrap() else {
        panic!("baseline")
    };
    assert_eq!(snapshot.checkpoint().reference(), &fixture.head);
    assert_eq!(snapshot.witness().reference(), &fixture.head);
    assert_eq!(snapshot.facts().cut, fixture.head.cut);
    assert!(snapshot.events().is_empty());
    let report = owner.shutdown();
    assert!(report.release.is_err());
    let store = Store::open(&fixture.path).unwrap();
    assert_eq!(
        store
            .restore_request(&fixture.session, "load")
            .unwrap()
            .unwrap()
            .phase,
        RestorePhase::Loading
    );
    assert!(
        store
            .get(&fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
}

#[test]
fn matching_completed_runtime_cleanup_releases_real_reservation() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    let scope = fixture.reserved(&mut owner);
    let loaded = fixture.load(&mut owner, scope);
    owner.register_loaded(&loaded).unwrap();
    assert!(matches!(
        receive(&mut owner).result.unwrap(),
        RestoreReply::CleanupRegistered
    ));
    loaded.close().unwrap();
    let report = owner.shutdown();
    report.operation.unwrap();
    assert_eq!(report.release.unwrap().unwrap().phase, RestorePhase::Closed);
}

#[test]
fn another_runtime_load_cannot_supply_cleanup_for_selected_scope() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    let mut scope = fixture.reserved(&mut owner);
    scope.load_id = ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let loaded = fixture.load(&mut owner, scope);
    owner.register_loaded(&loaded).unwrap();
    assert!(receive(&mut owner).result.is_err());
    loaded.close().unwrap();
    let report = owner.shutdown();
    assert!(report.operation.is_err());
    assert!(report.release.is_err());
}

#[test]
fn cancellation_is_sticky_and_persisted_without_starting_an_attempt() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    fixture.reserved(&mut owner);
    let cancelled = owner.take_cancellation().unwrap();
    owner.cancel(CancelMode::Hard);
    owner.cancel(CancelMode::Graceful);
    assert_eq!(
        cancelled.recv_timeout(Duration::from_secs(1)).unwrap(),
        CancelMode::Hard
    );
    let report = owner.shutdown();
    assert_eq!(
        report.release.unwrap().unwrap().phase,
        RestorePhase::Cancelled
    );
    assert!(
        Store::open(&fixture.path)
            .unwrap()
            .get(&fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
}

#[test]
fn sqlite_contention_cannot_delay_watchdog_or_revive_expired_owner() {
    let fixture = Fixture::new(Duration::from_millis(1500));
    let mut owner = fixture.start("load");
    fixture.reserved(&mut owner);
    let cancelled = owner.take_cancellation().unwrap();
    let locked = Connection::open(&fixture.path).unwrap();
    locked.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Instant::now();
    assert_eq!(
        cancelled.recv_timeout(Duration::from_secs(3)).unwrap(),
        CancelMode::Hard
    );
    assert!(owner.expired());
    assert!(start.elapsed() < Duration::from_secs(3));
    // Renewal is still blocked in SQLite while cancellation already reached supervision.
    locked.execute_batch("ROLLBACK").unwrap();
    let report = owner.shutdown();
    assert!(report.expired && report.operation.is_err() && report.release.is_err());
    assert!(
        Store::open(&fixture.path)
            .unwrap()
            .get(&fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
}

#[test]
fn metadata_retry_never_returns_a_second_loading_owner() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut first = fixture.start("load");
    fixture.reserved(&mut first);
    let mut retry = fixture.start("load");
    assert!(receive(&mut retry).result.is_err());
    assert!(retry.shutdown().release.unwrap().is_none());
    let report = first.shutdown();
    assert_eq!(report.release.unwrap().unwrap().phase, RestorePhase::Closed);
}

#[test]
fn same_scope_with_different_begin_binding_cannot_supply_cleanup() {
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    let scope = fixture.reserved(&mut owner);
    let (loaded, _cancelled) = fixture.load_pollable(&mut owner, scope, true);
    owner.register_loaded(&loaded).unwrap();
    assert!(receive(&mut owner).result.is_err());
    loaded.close().unwrap();
    assert!(owner.shutdown().release.is_err());
}

#[test]
fn blocked_sql_does_not_block_retained_child_cancellation_and_cleanup() {
    use foundry_runtime::coordinated::loading::CleanupStatus;
    let fixture = Fixture::new(Duration::from_millis(1500));
    let mut owner = fixture.start("load");
    let scope = fixture.reserved(&mut owner);
    let (mut loaded, cancelled) = fixture.load_pollable(&mut owner, scope, false);
    let observer = loaded.cleanup_observer();
    owner.register_loaded(&loaded).unwrap();
    assert!(matches!(
        receive(&mut owner).result.unwrap(),
        RestoreReply::CleanupRegistered
    ));
    let locked = Connection::open(&fixture.path).unwrap();
    locked.execute_batch("BEGIN IMMEDIATE").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if loaded.poll(&cancelled).is_err() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(owner.expired());
    assert_eq!(observer.status(), CleanupStatus::Complete);
    // The SQL writer is still locked when the actual process has been reaped.
    locked.execute_batch("ROLLBACK").unwrap();
    let report = owner.shutdown();
    assert!(report.expired && report.release.is_err());
}

#[test]
fn explicit_shutdown_waits_for_independently_supervised_cleanup() {
    use foundry_runtime::coordinated::loading::CleanupStatus;
    let fixture = Fixture::new(Duration::from_secs(4));
    let mut owner = fixture.start("load");
    let scope = fixture.reserved(&mut owner);
    let (mut loaded, cancelled) = fixture.load_pollable(&mut owner, scope, false);
    let observer = loaded.cleanup_observer();
    owner.register_loaded(&loaded).unwrap();
    assert!(matches!(
        receive(&mut owner).result.unwrap(),
        RestoreReply::CleanupRegistered
    ));
    let shutdown = thread::spawn(move || owner.shutdown());
    let deadline = Instant::now() + Duration::from_secs(3);
    while loaded.poll(&cancelled).is_ok() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(observer.status(), CleanupStatus::Complete);
    let report = shutdown.join().unwrap();
    report.operation.unwrap();
    assert_eq!(report.release.unwrap().unwrap().phase, RestorePhase::Closed);
}
