#![cfg(unix)]
use foundry_contracts::{CancelMode, CreateSession, Lifecycle, Outcome};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceOptions};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn options() -> ServiceOptions {
    ServiceOptions {
        lease_duration: Duration::from_millis(300),
        poll_interval: Duration::from_millis(20),
    }
}
fn create(service: &mut Service, directory: &Path) -> foundry_contracts::Session {
    service
        .create(CreateSession {
            prompt: "test task".into(),
            directory: directory.to_string_lossy().into(),
            model: None,
            idempotency_key: Some("test".into()),
        })
        .unwrap()
}
fn spec(mode: &str, marker: Option<&Path>) -> AdapterSpec {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .expect("Python 3 is required for offline fixtures");
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_adapter.py")
            .into_os_string(),
        mode.into(),
    ];
    if let Some(marker) = marker {
        spec.args.push(marker.as_os_str().into());
    }
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(50);
    spec
}
fn wait_marker(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "adapter did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn settled_result_and_events_survive_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut service = Service::open(&path).unwrap();
    let queued = create(&mut service, dir.path());
    let result = service
        .run(&queued.id, queued.version, &spec("success", None))
        .unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Exit);
    assert_eq!(result.outcome, Some(Outcome::Success));
    drop(service);
    let service = Service::open(&path).unwrap();
    assert_eq!(service.get(&queued.id).unwrap(), result);
    let events = service.events(&queued.id, 0, 20).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].kind, "adapter_settled");
    assert!(result.active_attempt_id.is_none());
}

#[test]
fn silent_adapter_retains_lease_independently_of_output() {
    let dir = tempdir().unwrap();
    let mut service = Service::open_with_options(dir.path().join("db"), options()).unwrap();
    let queued = create(&mut service, dir.path());
    let result = service.run(&queued.id, 0, &spec("quiet", None)).unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Exit);
}

#[test]
fn adapter_failure_is_durably_error_and_returns_error() {
    let dir = tempdir().unwrap();
    let mut service = Service::open(dir.path().join("db")).unwrap();
    let queued = create(&mut service, dir.path());
    assert!(service.run(&queued.id, 0, &spec("fail", None)).is_err());
    let failed = service.get(&queued.id).unwrap();
    assert_eq!(failed.lifecycle, Lifecycle::Error);
    assert!(failed.outcome.is_none());
    assert!(failed.active_attempt_id.is_none());
    let events = service.events(&queued.id, 0, 20).unwrap();
    let failures: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "adapter_failed")
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].data["code"], "adapter_failed");
    assert_eq!(
        failures[0].data["message"],
        "Adapter reported failure; details withheld"
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("fixture-private-diagnostic")
    );
}

#[test]
fn failed_event_write_cannot_be_reported_as_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut service = Service::open(&path).unwrap();
    let queued = create(&mut service, dir.path());
    let connection = rusqlite::Connection::open(path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(FAIL,'test write failure'); END;").unwrap();
    assert!(service.run(&queued.id, 0, &spec("success", None)).is_err());
    let failed = service.get(&queued.id).unwrap();
    assert_eq!(failed.lifecycle, Lifecycle::Error);
    assert!(failed.outcome.is_none());
    assert!(service.events(&queued.id, 0, 10).unwrap().is_empty());
}

#[test]
fn cancellation_helper() {
    let Some(path) = std::env::var_os("F5Y_TEST_CANCEL_DATABASE") else {
        return;
    };
    let id = std::env::var("F5Y_TEST_CANCEL_SESSION").unwrap();
    let mut service = Service::open(path).unwrap();
    service.cancel(&id, CancelMode::Hard).unwrap();
}

#[test]
fn another_process_cancels_a_silent_adapter() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let marker = dir.path().join("started");
    let mut service = Service::open_with_options(&path, options()).unwrap();
    let queued = create(&mut service, dir.path());
    let adapter = spec("wait", Some(&marker));
    let id = queued.id.clone();
    let runner = std::thread::spawn(move || service.run(&id, 0, &adapter));
    wait_marker(&marker);
    let started = Instant::now();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cancellation_helper"])
        .env("F5Y_TEST_CANCEL_DATABASE", &path)
        .env("F5Y_TEST_CANCEL_SESSION", &queued.id)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let result = runner.join().unwrap().unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Suspended);
    assert!(result.outcome.is_none());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        Service::open(path).unwrap().get(&queued.id).unwrap(),
        result
    );
}

#[test]
fn lease_loss_stops_adapter_and_requires_explicit_recovery() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let marker = dir.path().join("started");
    let mut service = Service::open_with_options(&path, options()).unwrap();
    let queued = create(&mut service, dir.path());
    let adapter = spec("wait", Some(&marker));
    let id = queued.id.clone();
    let runner = std::thread::spawn(move || service.run(&id, 0, &adapter));
    wait_marker(&marker);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    connection
        .execute(
            "UPDATE attempts SET expires_ms=0 WHERE session_id=?1",
            [&queued.id],
        )
        .unwrap();
    let started = Instant::now();
    assert!(runner.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
    let mut service = Service::open(path).unwrap();
    assert_eq!(
        service.get(&queued.id).unwrap().lifecycle,
        Lifecycle::Running
    );
    let recovered = service.recover_expired().unwrap();
    assert_eq!(recovered[0].lifecycle, Lifecycle::Suspended);
    assert!(recovered[0].outcome.is_none());
}

#[test]
fn failed_terminal_write_never_returns_adapter_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut service = Service::open(&path).unwrap();
    let queued = create(&mut service, dir.path());
    let connection = rusqlite::Connection::open(path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_finish BEFORE UPDATE OF body ON sessions WHEN json_extract(NEW.body,'$.lifecycle')='exit' BEGIN SELECT RAISE(FAIL,'test completion failure'); END;").unwrap();
    assert!(service.run(&queued.id, 0, &spec("success", None)).is_err());
    let session = service.get(&queued.id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Running);
    assert!(session.outcome.is_none());
    // A recorded adapter claim is not a committed session outcome.
    assert_eq!(
        service
            .events(&queued.id, 0, 10)
            .unwrap()
            .last()
            .unwrap()
            .kind,
        "adapter_settled"
    );
}
