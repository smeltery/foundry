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
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture did not reach barrier");
        thread::sleep(Duration::from_millis(5));
    }
}

fn adapter(directory: &Path, emit_text: bool) -> AdapterSpec {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    let code = r#"
import json, os, pathlib, sys, time
p = pathlib.Path(sys.argv[1])
print(json.dumps({'type': 'ready', 'version': 1}), flush=True)
a = json.loads(sys.stdin.readline())['attempt_id']
(p / 'pid').write_text(str(os.getpid()))
while not (p / 'go').exists(): time.sleep(.005)
if sys.argv[2] == 'text':
    print(json.dumps({'type': 'text', 'attempt_id': a, 'text': 'offline fixture'}), flush=True)
    (p / 'text_sent').write_text('sent')
time.sleep(1.4)
(p / 'effect').write_text('must not execute after expired ownership')
time.sleep(10)
"#;
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        code.into(),
        directory.as_os_str().into(),
        if emit_text { "text" } else { "silent" }.into(),
    ];
    spec.run_timeout = Duration::from_secs(10);
    spec
}

fn assert_expiry_stops_execution(emit_text: bool) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut service = Service::open_with_options(
        &path,
        ServiceOptions {
            lease_duration: Duration::from_secs(1),
            poll_interval: Duration::from_millis(20),
        },
    )
    .unwrap();
    let session = service
        .create(CreateSession {
            prompt: "offline lease test".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let spec = adapter(directory.path(), emit_text);
    let id = session.id.clone();
    let runner = thread::spawn(move || service.run(&id, 0, &spec));
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    wait_until(|| directory.path().join("pid").exists());
    wait_until(|| {
        connection
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    });
    // Block both heartbeat and (in the text case) synchronous event persistence.
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(directory.path().join("go"), "").unwrap();
    thread::sleep(Duration::from_millis(1600));
    let effect = directory.path().join("effect").exists();
    let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
    let alive = Command::new("kill")
        .args(["-0", &pid])
        .output()
        .unwrap()
        .status
        .success();
    // Release storage only after checking cleanup; joining may wait for this lock.
    connection.execute_batch("ROLLBACK").unwrap();
    let result = runner.join().unwrap();
    assert!(!effect, "adapter performed work after lease expiry");
    assert!(
        !alive,
        "adapter outlived its lease while storage was blocked"
    );
    assert!(
        result.is_err(),
        "uncertain ownership must not report success"
    );
    let mut reopened = Service::open(&path).unwrap();
    let uncertain = reopened.get(&session.id).unwrap();
    assert_eq!(uncertain.lifecycle, Lifecycle::Running);
    assert!(uncertain.outcome.is_none());
    assert!(uncertain.result.is_none());
    assert_eq!(
        reopened.recover_expired().unwrap()[0].lifecycle,
        Lifecycle::Suspended
    );
}

#[test]
fn blocked_heartbeat_cannot_extend_adapter_execution() {
    assert_expiry_stops_execution(false);
}

#[test]
fn blocked_event_callback_cannot_defeat_lease_watchdog() {
    assert_expiry_stops_execution(true);
}

fn assert_local_cancel_interrupts_write(reject_write: bool) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut service = Service::open_with_options(
        &path,
        ServiceOptions {
            lease_duration: Duration::from_secs(15),
            poll_interval: Duration::from_millis(20),
        },
    )
    .unwrap();
    let session = service
        .create(CreateSession {
            prompt: "offline cancellation".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let spec = adapter(directory.path(), true);
    let control = Arc::new(AtomicU8::new(0));
    let worker_control = Arc::clone(&control);
    let id = session.id.clone();
    let runner = thread::spawn(move || service.run_with_control(&id, 0, &spec, worker_control));
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    wait_until(|| directory.path().join("pid").exists());
    wait_until(|| {
        connection
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    });
    if reject_write {
        connection.execute_batch("CREATE TRIGGER reject_text BEFORE INSERT ON events WHEN NEW.kind='text' BEGIN SELECT RAISE(FAIL,'offline failure'); END;").unwrap();
    }
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(directory.path().join("go"), "").unwrap();
    wait_until(|| directory.path().join("text_sent").exists());
    // Let the event callback and monitor enter their blocked database operations.
    thread::sleep(Duration::from_millis(100));
    control.store(2, Ordering::Release);
    let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
    let deadline = Instant::now() + Duration::from_millis(700);
    let stopped = loop {
        if !Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap()
            .status
            .success()
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(5));
    };
    connection.execute_batch("ROLLBACK").unwrap();
    let result = runner.join().unwrap();
    assert!(
        stopped,
        "hard cancellation waited for blocked persistence or the lease budget"
    );
    assert_eq!(
        result.is_err(),
        reject_write,
        "worker write errors must survive cancellation"
    );
    let reopened = Service::open(&path).unwrap();
    let session = reopened.get(&session.id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Suspended);
    assert!(session.active_attempt_id.is_none());
    assert!(session.outcome.is_none());
    assert!(!directory.path().join("effect").exists());
}

#[test]
fn local_hard_cancel_interrupts_blocked_event_write() {
    assert_local_cancel_interrupts_write(false);
}

#[test]
fn abandoned_event_wait_preserves_worker_write_failure() {
    assert_local_cancel_interrupts_write(true);
}
