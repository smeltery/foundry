#![cfg(unix)]

use foundry_contracts::Lifecycle;
use foundry_store::Store;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const ADAPTER: &str = r#"
import json, os, pathlib, subprocess, sys, time
def emit(value):
    print(json.dumps(value), flush=True)
emit({'type':'ready','version':1})
start = json.loads(sys.stdin.readline())
child = subprocess.Popen([sys.executable, '-u', '-c',
    'import os,time; print(os.getpid(),flush=True); time.sleep(30)'], stdout=subprocess.PIPE)
descendant = int(child.stdout.readline())
child.stdout.close()
metadata = {'session':start['session']['id'], 'adapter':os.getpid(), 'descendant':descendant}
pathlib.Path('ready.tmp').write_text(json.dumps(metadata))
os.replace('ready.tmp', 'ready.json')
emit({'type':'text','attempt_id':start['attempt_id'],'text':'fixture ready'})
for line in sys.stdin:
    command = json.loads(line)
    if command['type'] == 'cancel':
        pathlib.Path('cancel.tmp').write_text(command['mode'])
        os.replace('cancel.tmp', 'cancelled')
        if start['session']['prompt'] != 'ignore graceful':
            emit({'type':'settled','attempt_id':start['attempt_id'],
                  'outcome':'success','result':'must not be reported as success'})
            break
"#;

struct Running {
    directory: TempDir,
    child: Option<Child>,
    metadata: Value,
}

struct Watcher(Child);

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Running {
    fn start(prompt: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap();
        let script = directory.path().join("adapter.py");
        fs::write(&script, format!("#!{}\n{}", python.trim(), ADAPTER)).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let child = cli(directory.path())
            .env("FOUNDRY_ADAPTER", &script)
            .args(["run", "--task", prompt])
            .spawn()
            .unwrap();
        let mut running = Self {
            directory,
            child: Some(child),
            metadata: Value::Null,
        };
        wait_until(|| running.directory.path().join("ready.json").is_file());
        running.metadata =
            serde_json::from_slice(&fs::read(running.directory.path().join("ready.json")).unwrap())
                .unwrap();
        running
    }

    fn id(&self) -> &str {
        self.metadata["session"].as_str().unwrap()
    }

    fn signal(&self, signal: &str) {
        signal_pid(self.child.as_ref().unwrap().id(), signal);
    }

    fn finish(&mut self) -> Output {
        wait_until(|| self.child.as_mut().unwrap().try_wait().unwrap().is_some());
        self.child.take().unwrap().wait_with_output().unwrap()
    }

    fn store(&self) -> Store {
        Store::open(self.directory.path().join("state.db")).unwrap()
    }

    fn assert_stopped(&self) {
        let session = self.store().get(self.id()).unwrap();
        assert_eq!(session.lifecycle, Lifecycle::Suspended);
        assert!(session.outcome.is_none());
        assert!(session.active_attempt_id.is_none());
        for key in ["adapter", "descendant"] {
            let pid = self.metadata[key].as_u64().unwrap();
            wait_until(|| !process_running(pid));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // Failure cleanup: kill only this fixture's still-owned process group.
            if child.try_wait().ok().flatten().is_none() {
                if let Some(pid) = self.metadata["adapter"].as_u64() {
                    let _ = Command::new("python3")
                        .args([
                            "-c",
                            "import os,signal,sys; os.killpg(int(sys.argv[1]), signal.SIGKILL)",
                            &pid.to_string(),
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn cli(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
    command
        .current_dir(directory)
        .env_remove("FOUNDRY_DATABASE")
        .env_remove("FOUNDRY_ADAPTER")
        .args(["--database", "state.db", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "fixture deadline exceeded"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn signal_pid(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

fn process_running(pid: u64) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8(output.stdout).unwrap();
    // Orphan zombies may wait for the host's init process; they cannot execute.
    !state.trim().is_empty() && !state.trim().starts_with('Z')
}

#[test]
fn sigint_and_sigterm_suspend_owned_execution_and_stop_descendants() {
    for signal in ["-INT", "-TERM"] {
        let mut running = Running::start("graceful");
        running.signal(signal);
        let output = running.finish();
        assert_eq!(
            output.status.code(),
            Some(130),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["session"]["lifecycle"], "suspended");
        assert!(response["session"]["outcome"].is_null());
        assert_eq!(
            fs::read_to_string(running.directory.path().join("cancelled")).unwrap(),
            "graceful"
        );
        running.assert_stopped();
    }
}

#[test]
fn second_signal_escalates_after_graceful_request_without_waiting_for_timeout() {
    let mut running = Running::start("ignore graceful");
    running.signal("-INT");
    wait_until(|| running.directory.path().join("cancelled").exists());
    let escalation = Instant::now();
    running.signal("-TERM");
    let output = running.finish();
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(escalation.elapsed() < Duration::from_secs(2));
    running.assert_stopped();
}

#[test]
fn interrupted_watch_returns_130_without_cancelling_producer() {
    let mut producer = Running::start("graceful");
    let before = producer.store().get(producer.id()).unwrap();
    let mut watcher = Watcher(
        cli(producer.directory.path())
            .args(["watch", producer.id(), "--interval", "60"])
            .spawn()
            .unwrap(),
    );
    let stdout = watcher.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let result = reader.read_line(&mut line);
        let _ = ready_tx.send((result, line));
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let (result, line) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.unwrap() > 0);
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["type"],
        "event"
    );
    let interrupted = Instant::now();
    signal_pid(watcher.0.id(), "-TERM");
    wait_until(|| watcher.0.try_wait().unwrap().is_some());
    assert_eq!(watcher.0.wait().unwrap().code(), Some(130));
    assert!(interrupted.elapsed() < Duration::from_secs(2));
    reader.join().unwrap();
    let after = producer.store().get(producer.id()).unwrap();
    assert_eq!(after.lifecycle, Lifecycle::Running);
    assert_eq!(after.active_attempt_id, before.active_attempt_id);
    assert!(!producer.directory.path().join("cancelled").exists());
    assert!(
        producer
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
    );
    producer.signal("-INT");
    assert_eq!(producer.finish().status.code(), Some(130));
    producer.assert_stopped();
}
