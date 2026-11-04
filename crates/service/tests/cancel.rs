#![cfg(unix)]
use foundry_contracts::{CancelMode, CancelReceipt, ContractError, CreateSession, Lifecycle};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceError, ServiceOptions};
use foundry_store::StoreError;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn options() -> ServiceOptions {
    ServiceOptions {
        lease_duration: Duration::from_secs(2),
        poll_interval: Duration::from_millis(20),
    }
}

fn adapter(marker: &Path) -> AdapterSpec {
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
        "wait".into(),
        marker.as_os_str().into(),
    ];
    spec.run_timeout = Duration::from_secs(20);
    // Graceful alone cannot settle within the test; only the hard upgrade can.
    spec.cancel_timeout = Duration::from_secs(15);
    spec
}

fn wait_marker(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "adapter did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn receipt(requested: bool, mode: CancelMode, status: Lifecycle) -> CancelReceipt {
    CancelReceipt {
        ok: true,
        requested,
        mode,
        status,
    }
}

#[test]
fn graceful_then_hard_upgrade_settles_suspended_and_a_later_cancel_is_terminal() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let marker = dir.path().join("started");
    let mut service = Service::open_with_options(&path, options()).unwrap();
    let session = service
        .create(CreateSession {
            prompt: "cancel fixture".into(),
            directory: dir.path().to_string_lossy().into(),
            model: None,
            idempotency_key: None,
            ..Default::default()
        })
        .unwrap();
    let spec = adapter(&marker);
    let id = session.id.clone();
    let runner = std::thread::spawn(move || service.run(&id, 0, &spec));
    wait_marker(&marker);
    let mut control = Service::open_with_options(&path, options()).unwrap();
    let started = Instant::now();
    assert_eq!(
        control.cancel(&session.id, CancelMode::Graceful).unwrap(),
        receipt(true, CancelMode::Graceful, Lifecycle::Running)
    );
    assert_eq!(
        control.cancel(&session.id, CancelMode::Hard).unwrap(),
        receipt(true, CancelMode::Hard, Lifecycle::Running)
    );
    let settled = runner.join().unwrap().unwrap();
    assert_eq!(settled.lifecycle, Lifecycle::Suspended);
    assert!(settled.outcome.is_none());
    assert!(started.elapsed() < Duration::from_secs(10));

    assert_eq!(
        control.cancel(&session.id, CancelMode::Graceful).unwrap(),
        receipt(true, CancelMode::Graceful, Lifecycle::Cancelled)
    );
    std::fs::remove_file(&marker).unwrap();
    let cancelled = control.get(&session.id).unwrap();
    let refused = control.run(&session.id, cancelled.version, &adapter(&marker));
    assert!(
        matches!(
            refused,
            Err(ServiceError::Store(StoreError::Contract(
                ContractError::Cancelled(_)
            )))
        ),
        "{refused:?}"
    );
    assert!(!marker.exists(), "a cancelled session launched an adapter");
    assert_eq!(control.get(&session.id).unwrap(), cancelled);
}
