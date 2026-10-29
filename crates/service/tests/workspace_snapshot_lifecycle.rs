#![cfg(unix)]
use foundry_contracts::{CreateSession, Lifecycle};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceOptions, cancellation_snapshot::SnapshotSettings};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    workspace: PathBuf,
    settings: SnapshotSettings,
}
impl Fixture {
    fn new() -> Self {
        let helper = PathBuf::from(
            std::env::var_os("FOUNDRY_SNAPSHOT_TEST_HELPER")
                .expect("provide the rebuilt fixed f5y-toolhost"),
        );
        assert!(helper.is_absolute() && helper.is_file());
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        fs::write(workspace.join("note"), "uncommitted workspace content").unwrap();
        let database = root.path().canonicalize().unwrap().join("state.db");
        let settings = SnapshotSettings::for_database(helper, &database).unwrap();
        Self {
            root,
            database,
            workspace,
            settings,
        }
    }
    fn service(&self) -> (Service, String) {
        let mut service = Service::open_with_options(
            &self.database,
            ServiceOptions {
                lease_duration: Duration::from_secs(3),
                poll_interval: Duration::from_millis(20),
            },
        )
        .unwrap()
        .with_workspace_snapshots(self.settings.clone());
        let session = service
            .create(CreateSession {
                prompt: "snapshot after cancellation".into(),
                directory: self.workspace.display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        (service, session.id)
    }
    fn events(&self, id: &str) -> Vec<Value> {
        Service::open(&self.database)
            .unwrap()
            .events(id, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "workspace_snapshot")
            .map(|event| event.data)
            .collect()
    }
    fn require_event_before_suspension(&self) {
        rusqlite::Connection::open(&self.database)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER require_snapshot BEFORE UPDATE OF body ON sessions
             WHEN json_extract(NEW.body,'$.lifecycle')='suspended'
             BEGIN SELECT CASE WHEN NOT EXISTS(
                 SELECT 1 FROM events WHERE session_id=NEW.id AND kind='workspace_snapshot'
             ) THEN RAISE(ABORT,'snapshot event required before suspension') END; END;",
            )
            .unwrap();
    }
    fn run_before_spawn(&self, level: u8) -> (String, foundry_contracts::Session) {
        let (mut service, id) = self.service();
        self.require_event_before_suspension();
        let session = service
            .run_with_control(
                &id,
                0,
                &AdapterSpec::new("/missing/never-spawn"),
                Arc::new(AtomicU8::new(level)),
            )
            .unwrap();
        assert_eq!(session.lifecycle, Lifecycle::Suspended);
        assert!(session.active_attempt_id.is_none() && session.outcome.is_none());
        (id, session)
    }
    fn no_artifact(&self) {
        assert_eq!(fs::read_dir(&self.settings.destination).unwrap().count(), 0);
    }
}

fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "adapter did not start");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn graceful_cleanup_records_real_artifact_before_suspension() {
    let fixture = Fixture::new();
    let (mut service, id) = fixture.service();
    fixture.require_event_before_suspension();
    let marker = fixture.root.path().join("adapter.pid");
    let mut spec = AdapterSpec::new("/usr/bin/python3");
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("control_adapter.py").into(),
        "silent".into(),
        marker.clone().into_os_string(),
    ];
    spec.cancel_timeout = Duration::from_millis(100);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = control.clone();
    let run_id = id.clone();
    let worker = thread::spawn(move || service.run_with_control(&run_id, 0, &spec, worker_control));
    wait_file(&marker);
    control.store(1, Ordering::Release);
    let session = worker.join().unwrap().unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Suspended);
    assert!(session.active_attempt_id.is_none() && session.outcome.is_none());
    let pid: i32 = fs::read_to_string(marker).unwrap().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let events = fixture.events(&id);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event["status"], "captured");
    assert_eq!(event["atomic"], false);
    let artifact = fixture
        .settings
        .destination
        .join(event["artifact"].as_str().unwrap());
    assert_eq!(
        fs::read(artifact.join("tree/note")).unwrap(),
        fs::read(fixture.workspace.join("note")).unwrap()
    );
    let digest: String = Sha256::digest(fs::read(artifact.join("manifest.jsonl")).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(event["manifest_digest"], digest);
    assert_eq!(
        fs::read_dir(&fixture.settings.destination).unwrap().count(),
        1
    );
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn hard_cancel_records_skip_without_copy_or_spawn() {
    let fixture = Fixture::new();
    let (id, _) = fixture.run_before_spawn(2);
    let events = fixture.events(&id);
    assert_eq!(
        events,
        vec![serde_json::json!({"status":"failed","code":"aborted_hard"})]
    );
    fixture.no_artifact();
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn copy_limit_is_truthful_failed_event_not_captured_success() {
    let mut fixture = Fixture::new();
    fixture.settings.limits.bytes = 1;
    let (id, _) = fixture.run_before_spawn(1);
    assert_eq!(
        fixture.events(&id),
        vec![serde_json::json!({"status":"failed","code":"limit"})]
    );
    fixture.no_artifact();
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn runtime_failure_never_enters_snapshot_copy() {
    let fixture = Fixture::new();
    let (mut service, id) = fixture.service();
    assert!(
        service
            .run(&id, 0, &AdapterSpec::new("/missing/never-spawn"))
            .is_err()
    );
    assert!(fixture.events(&id).is_empty());
    fixture.no_artifact();
    assert_eq!(service.get(&id).unwrap().lifecycle, Lifecycle::Error);
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn failed_snapshot_event_commit_cannot_return_successful_suspension() {
    let fixture = Fixture::new();
    let (mut service, id) = fixture.service();
    rusqlite::Connection::open(&fixture.database)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_snapshot BEFORE INSERT ON events
         WHEN NEW.kind='workspace_snapshot' BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        )
        .unwrap();
    assert!(
        service
            .run_with_control(
                &id,
                0,
                &AdapterSpec::new("/missing/never-spawn"),
                Arc::new(AtomicU8::new(1))
            )
            .is_err()
    );
    assert!(fixture.events(&id).is_empty());
    let session = service.get(&id).unwrap();
    assert!(session.active_attempt_id.is_some());
    assert_ne!(session.lifecycle, Lifecycle::Suspended);
}
