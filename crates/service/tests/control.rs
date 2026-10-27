#![cfg(unix)]
use foundry_contracts::{CreateSession, Lifecycle};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceOptions};
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn setup(path: &Path, directory: &Path) -> (Service, String) {
    let mut service = Service::open_with_options(
        path,
        ServiceOptions {
            lease_duration: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    let session = service
        .create(CreateSession {
            prompt: "test".into(),
            directory: directory.to_string_lossy().into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    (service, session.id)
}
fn adapter(mode: &str, marker: &Path) -> AdapterSpec {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = ["-u", "-c", include_str!("control_adapter.py"), mode]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.args.push(marker.as_os_str().into());
    spec.run_timeout = Duration::from_secs(15);
    spec.cancel_timeout = Duration::from_millis(100);
    spec
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !path.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn cancelled_mode(path: &Path, id: &str) -> String {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT cancel FROM attempts WHERE session_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}
fn assert_clean(marker: &Path) {
    let pid = std::fs::read_to_string(marker).unwrap();
    let result = Command::new("kill").args(["-0", &pid]).output().unwrap();
    assert!(
        !result.status.success(),
        "adapter still running after service returned"
    );
}

#[test]
fn cancellation_before_spawn_never_launches_adapter() {
    for (level, mode) in [(1, "graceful"), (2, "hard")] {
        let directory = tempdir().unwrap();
        let path = directory.path().join("db");
        let (mut service, id) = setup(&path, directory.path());
        let result = service
            .run_with_control(
                &id,
                0,
                &AdapterSpec::new("/missing/never-spawn"),
                Arc::new(AtomicU8::new(level)),
            )
            .unwrap();
        assert_eq!(result.lifecycle, Lifecycle::Suspended);
        assert!(result.active_attempt_id.is_none());
        assert_eq!(cancelled_mode(&path, &id), mode);
        assert!(service.events(&id, 0, 10).unwrap().is_empty());
    }
}

#[test]
fn local_graceful_control_cancels_silent_adapter_and_persists_cleanup() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    let marker = directory.path().join("pid");
    let spec = adapter("silent", &marker);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let run_id = id.clone();
    let runner =
        std::thread::spawn(move || service.run_with_control(&run_id, 0, &spec, worker_control));
    wait_file(&marker);
    control.fetch_max(1, Ordering::AcqRel);
    let result = runner.join().unwrap().unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Suspended);
    assert!(result.outcome.is_none());
    assert!(result.active_attempt_id.is_none());
    assert_eq!(cancelled_mode(&path, &id), "graceful");
    assert_clean(&marker);
    assert_eq!(Service::open(path).unwrap().get(&id).unwrap(), result);
}

#[test]
fn second_control_escalates_before_graceful_deadline() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    let marker = directory.path().join("pid");
    let mut spec = adapter("upgrade", &marker);
    spec.cancel_timeout = Duration::from_secs(10);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let run_id = id.clone();
    let runner =
        std::thread::spawn(move || service.run_with_control(&run_id, 0, &spec, worker_control));
    wait_file(&marker);
    control.fetch_max(1, Ordering::AcqRel);
    wait_file(&marker.with_extension("graceful"));
    let started = Instant::now();
    control.fetch_max(2, Ordering::AcqRel);
    let result = runner.join().unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(result.lifecycle, Lifecycle::Suspended);
    assert_eq!(cancelled_mode(&path, &id), "hard");
    assert_clean(&marker);
}

#[test]
fn success_after_observed_cancellation_is_suppressed() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    let marker = directory.path().join("pid");
    let spec = adapter("late_success", &marker);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let run_id = id.clone();
    let runner =
        std::thread::spawn(move || service.run_with_control(&run_id, 0, &spec, worker_control));
    wait_file(&marker);
    control.fetch_max(1, Ordering::AcqRel);
    let result = runner.join().unwrap().unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Suspended);
    assert!(result.outcome.is_none());
    assert!(result.result.is_none());
    assert!(
        !Service::open(path)
            .unwrap()
            .events(&id, 0, 100)
            .unwrap()
            .iter()
            .any(|event| event.kind == "adapter_settled")
    );
    assert_clean(&marker);
}
