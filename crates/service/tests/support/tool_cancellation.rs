use super::{Fixture, support};
use foundry_contracts::Lifecycle;
use foundry_service::{Service, ServiceOptions};
use foundry_store::Store;
use foundry_tools::host::Grant;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[test]
fn lease_and_local_cancel_stop_both_processes_while_sqlite_event_write_is_blocked() {
    for expire in [false, true] {
        let mut fixture = Fixture::new(
            "wait",
            ServiceOptions {
                lease_duration: Duration::from_secs(if expire { 1 } else { 15 }),
                poll_interval: Duration::from_millis(20),
            },
        );
        let policy = fixture.policy(vec![Grant::Read]);
        let spec = fixture.adapter("text_after_helper");
        let control = Arc::new(AtomicU8::new(0));
        let worker_control = Arc::clone(&control);
        let id = fixture.session.clone();
        let root = fixture.directory.path().to_path_buf();
        let connection = rusqlite::Connection::open(&fixture.database).unwrap();
        connection.busy_timeout(Duration::from_secs(2)).unwrap();
        let runner = thread::spawn(move || {
            fixture
                .service
                .run_with_tools(&id, 0, &spec, worker_control, policy)
        });
        support::wait(|| root.join("helper_started").exists());
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        std::fs::write(root.join("text_release"), "").unwrap();
        support::wait(|| root.join("text_sent").exists());
        thread::sleep(Duration::from_millis(80));
        if !expire {
            control.store(2, Ordering::Release);
        }
        let deadline = Instant::now() + Duration::from_millis(if expire { 1100 } else { 500 });
        let stopped = loop {
            if !support::alive(&root.join("helper_started"))
                && !support::alive(&root.join("adapter_started"))
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(5));
        };
        if expire {
            // Keep storage locked past durable expiry, not just the conservative cutoff.
            thread::sleep(Duration::from_millis(1100));
        }
        connection.execute_batch("ROLLBACK").unwrap();
        let result = runner.join().unwrap();
        assert!(stopped, "owned processes did not stop with storage locked");
        assert!(!root.join("effect").exists());
        assert!(!root.join("observed").exists());
        let record = Store::open(&fixture.database)
            .unwrap()
            .tool_call(&fixture.session, "call")
            .unwrap()
            .unwrap();
        assert!(record.outcome.is_none());
        let mut service = Service::open(&fixture.database).unwrap();
        if expire {
            assert!(result.is_err());
            assert_eq!(
                service.recover_expired().unwrap()[0].lifecycle,
                Lifecycle::Suspended
            );
        } else {
            // A storage failure after cancellation remains an error; never success/Exit.
            if let Ok(session) = result {
                assert_eq!(session.lifecycle, Lifecycle::Suspended);
            }
            assert_eq!(
                service.get(&fixture.session).unwrap().lifecycle,
                Lifecycle::Suspended
            );
        }
    }
}

#[test]
fn cancellation_before_tool_event_commit_never_spawns_queued_helper() {
    let mut fixture = Fixture::new("success", ServiceOptions::default());
    let spec = fixture.adapter("call_barrier");
    let policy = fixture.policy(vec![Grant::Read]);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let id = fixture.session.clone();
    let root = fixture.directory.path().to_path_buf();
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    let runner = thread::spawn(move || {
        fixture
            .service
            .run_with_tools(&id, 0, &spec, worker_control, policy)
    });
    support::wait(|| root.join("adapter_started").exists());
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(root.join("call_release"), "").unwrap();
    thread::sleep(Duration::from_millis(100));
    control.store(2, Ordering::Release);
    support::wait(|| !support::alive(&root.join("adapter_started")));
    connection.execute_batch("ROLLBACK").unwrap();
    let _ = runner.join().unwrap();
    assert!(!root.join("helper_started").exists());
    assert!(!root.join("observed").exists());
    assert_eq!(
        Service::open(&fixture.database)
            .unwrap()
            .get(&fixture.session)
            .unwrap()
            .lifecycle,
        Lifecycle::Suspended
    );
}

#[test]
fn cancellation_during_receipt_write_never_releases_adapter_result() {
    let mut fixture = Fixture::new("barrier", ServiceOptions::default());
    let spec = fixture.adapter("read");
    let policy = fixture.policy(vec![Grant::Read]);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let id = fixture.session.clone();
    let root = fixture.directory.path().to_path_buf();
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    let runner = thread::spawn(move || {
        fixture
            .service
            .run_with_tools(&id, 0, &spec, worker_control, policy)
    });
    support::wait(|| root.join("helper_started").exists());
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(root.join("release"), "").unwrap();
    support::wait(|| !support::alive(&root.join("helper_started")));
    thread::sleep(Duration::from_millis(80));
    control.store(2, Ordering::Release);
    support::wait(|| !support::alive(&root.join("adapter_started")));
    connection.execute_batch("ROLLBACK").unwrap();
    let _ = runner.join().unwrap();
    assert!(!root.join("observed").exists());
    assert_eq!(
        Service::open(&fixture.database)
            .unwrap()
            .get(&fixture.session)
            .unwrap()
            .lifecycle,
        Lifecycle::Suspended
    );
}
