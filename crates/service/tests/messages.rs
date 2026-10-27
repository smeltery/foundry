#![cfg(unix)]
use foundry_contracts::{CancelMode, CreateSession, Lifecycle, MessageState};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceOptions};
use std::{
    path::Path,
    process::Command,
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
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(output.stdout).unwrap().trim());
    spec.args = ["-u", "-c", include_str!("message_adapter.py"), mode]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.args.push(marker.as_os_str().into());
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(50);
    spec
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !path.exists() {
        assert!(Instant::now() < deadline, "adapter did not reach marker");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn duplicate_ack_is_durable_once_and_restart_retains_delivery() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "instruction").unwrap();
    service
        .run(&id, 0, &adapter("ack", &directory.path().join("ready")))
        .unwrap();
    drop(service);
    let service = Service::open(path).unwrap();
    let messages = service.messages(&id, 0, 10).unwrap();
    assert_eq!(messages[0].state, MessageState::Delivered);
    assert_eq!(
        service
            .events(&id, 0, 100)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "message_accepted")
            .count(),
        1
    );
}

#[test]
fn adapter_without_capability_leaves_message_pending_and_withdrawable() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "instruction").unwrap();
    service
        .run(
            &id,
            0,
            &adapter("no_capability", &directory.path().join("ready")),
        )
        .unwrap();
    assert_eq!(
        service.messages(&id, 0, 10).unwrap()[0].state,
        MessageState::Pending
    );
    assert_eq!(
        service.withdraw_message(&id, "m").unwrap().state,
        MessageState::Withdrawn
    );
}

#[test]
fn enqueue_helper() {
    let Some(path) = std::env::var_os("F5Y_TEST_MESSAGE_DB") else {
        return;
    };
    let id = std::env::var("F5Y_TEST_MESSAGE_SESSION").unwrap();
    Service::open(path)
        .unwrap()
        .enqueue_message(&id, "m", "from another process")
        .unwrap();
}

#[test]
fn running_adapter_receives_message_enqueued_by_another_process() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    let marker = directory.path().join("ready");
    let spec = adapter("ack", &marker);
    let run_id = id.clone();
    let runner = std::thread::spawn(move || service.run(&run_id, 0, &spec));
    wait_file(&marker);
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "enqueue_helper"])
        .env("F5Y_TEST_MESSAGE_DB", &path)
        .env("F5Y_TEST_MESSAGE_SESSION", &id)
        .output()
        .unwrap();
    assert!(child.status.success());
    assert_eq!(runner.join().unwrap().unwrap().lifecycle, Lifecycle::Exit);
    assert_eq!(
        Service::open(path).unwrap().messages(&id, 0, 10).unwrap()[0].state,
        MessageState::Delivered
    );
}

#[test]
fn inflight_withdrawal_fails_and_cancellation_retains_ambiguity() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "instruction").unwrap();
    let marker = directory.path().join("ready");
    let spec = adapter("unacknowledged", &marker);
    let run_id = id.clone();
    let runner = std::thread::spawn(move || service.run(&run_id, 0, &spec));
    wait_file(&marker.with_extension("received"));
    let mut other = Service::open(&path).unwrap();
    assert!(other.withdraw_message(&id, "m").is_err());
    other.cancel(&id, CancelMode::Hard).unwrap();
    assert_eq!(
        runner.join().unwrap().unwrap().lifecycle,
        Lifecycle::Suspended
    );
    assert_eq!(
        other.messages(&id, 0, 10).unwrap()[0].state,
        MessageState::InFlight
    );
}

#[test]
fn adapter_crash_never_marks_inflight_message_delivered() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "instruction").unwrap();
    assert!(
        service
            .run(&id, 0, &adapter("crash", &directory.path().join("ready")))
            .is_err()
    );
    drop(service);
    let service = Service::open(path).unwrap();
    assert_eq!(
        service.messages(&id, 0, 10).unwrap()[0].state,
        MessageState::InFlight
    );
    assert_eq!(service.get(&id).unwrap().lifecycle, Lifecycle::Error);
}

#[test]
fn failed_acknowledgment_write_stops_run_without_claiming_delivery() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "instruction").unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_delivery BEFORE UPDATE OF state ON messages WHEN NEW.state='delivered' BEGIN SELECT RAISE(FAIL,'test delivery failure'); END;").unwrap();
    assert!(
        service
            .run(&id, 0, &adapter("ack", &directory.path().join("ready")))
            .is_err()
    );
    assert_eq!(
        service.messages(&id, 0, 10).unwrap()[0].state,
        MessageState::InFlight
    );
    assert_eq!(service.get(&id).unwrap().lifecycle, Lifecycle::Error);
}

#[test]
fn missing_malformed_or_wrong_receipt_never_marks_message_delivered() {
    for mode in ["missing_receipt", "malformed_receipt", "wrong_receipt"] {
        let directory = tempdir().unwrap();
        let path = directory.path().join("db");
        let (mut service, id) = setup(&path, directory.path());
        service.enqueue_message(&id, "m", "instruction").unwrap();
        assert!(
            service
                .run(&id, 0, &adapter(mode, &directory.path().join("ready")))
                .is_err(),
            "{mode}"
        );
        assert_eq!(
            service.messages(&id, 0, 10).unwrap()[0].state,
            MessageState::InFlight
        );
        assert_eq!(service.get(&id).unwrap().lifecycle, Lifecycle::Error);
        assert!(
            !service
                .events(&id, 0, 100)
                .unwrap()
                .iter()
                .any(|event| event.kind == "message_accepted")
        );
    }
}
