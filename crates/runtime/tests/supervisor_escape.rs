#![cfg(unix)]
//! Hard cancellation must also stop adapter descendants that leave the owned
//! process group. Every wait is bounded by the fixture's own readiness marker:
//! the escaped descendant holds the only write end of a FIFO, so its pid line
//! proves it escaped and end-of-file proves it exited.

use foundry_contracts::{AdapterEvent, CancelMode, Lifecycle, Session};
use foundry_runtime::{AdapterSpec, RunResult, run_adapter};
use std::io::{ErrorKind, Read};
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ADAPTER: &str = r#"
import json, os, signal, sys
fifo, mode = sys.argv[1], sys.argv[2]
def emit(payload):
    print(json.dumps(payload), flush=True)
emit({"type": "ready", "version": 1})
attempt = json.loads(sys.stdin.readline())["attempt_id"]
pid = os.fork()
if pid == 0:
    # A daemon-like escapee: no hangup or termination request stops it.
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.setsid()
    if mode != "setsid" and os.fork():
        if mode == "double_fork":
            os._exit(0)
        while True:
            signal.pause()
    marker = os.open(fifo, os.O_WRONLY)
    os.write(marker, f"{os.getpid()} {os.getsid(0)} {os.getpgid(0)}\n".encode())
    while True:
        signal.pause()
if mode == "double_fork":
    os.waitpid(pid, 0)
emit({"type": "text", "attempt_id": attempt, "text": str(pid)})
sys.stdin.readline()
"#;
// Generous: these deadlines bound fixture work, never the cleanup under test.
const MARKER_DEADLINE: Duration = Duration::from_secs(10);

struct Fixture {
    directory: PathBuf,
    marker: std::fs::File,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("f5y-escape-{mode}-{}-{suffix}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let fifo = directory.join("marker");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        // A reader must exist before the descendant's blocking write-side open.
        let marker = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        Self { directory, marker }
    }

    fn fifo(&self) -> PathBuf {
        self.directory.join("marker")
    }

    /// Returns the escaped descendant's pid, session and group once it holds the FIFO.
    fn escaped_identity(&mut self) -> [i32; 3] {
        let deadline = Instant::now() + MARKER_DEADLINE;
        let mut line = Vec::new();
        while !line.ends_with(b"\n") {
            let mut byte = [0_u8; 1];
            match self.marker.read(&mut byte) {
                Ok(1) => {
                    line.push(byte[0]);
                    continue;
                }
                // No writer has opened the FIFO yet, or it has not written.
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("marker read failed: {error}"),
            }
            assert!(Instant::now() < deadline, "descendant never became ready");
            std::thread::sleep(Duration::from_millis(2));
        }
        let fields: Vec<i32> = std::str::from_utf8(&line)
            .unwrap()
            .split_whitespace()
            .map(|field| field.parse().unwrap())
            .collect();
        fields.try_into().unwrap()
    }

    /// End-of-file after the pid line: the descendant's last descriptor closed.
    fn descendant_exited(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let mut byte = [0_u8; 1];
            match self.marker.read(&mut byte) {
                Ok(0) => return true,
                Ok(_) => panic!("unexpected marker output"),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("marker read failed: {error}"),
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn spec(fifo: &Path, mode: &str) -> AdapterSpec {
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .expect("Python 3 is required for offline adapter fixtures");
    assert!(output.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(output.stdout).unwrap().trim());
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        ADAPTER.into(),
        fifo.into(),
        mode.into(),
    ];
    spec.ready_timeout = MARKER_DEADLINE;
    spec.run_timeout = MARKER_DEADLINE;
    spec
}

fn session(directory: &Path) -> Session {
    Session {
        id: "session".into(),
        prompt: "offline fixture".into(),
        directory: directory.to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("attempt".into()),
        version: 1,
    }
}

fn hard_cancel_reaps_escaped(mode: &str) {
    let mut fixture = Fixture::new(mode);
    let spec = spec(&fixture.fifo(), mode);
    let session = session(&fixture.directory);
    let (sender, cancel) = mpsc::channel();
    let mut escaped = None;
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        let AdapterEvent::Text { text, .. } = event else {
            return;
        };
        let identity = fixture.escaped_identity();
        // Still holding the FIFO: alive when hard cancellation is requested.
        let alive = !fixture.descendant_exited(Duration::ZERO);
        escaped = Some((text.parse::<i32>().unwrap(), identity, alive));
        sender.send(CancelMode::Hard).unwrap();
    });
    let (forked, [pid, sid, pgid], alive) =
        escaped.expect("descendant escaped before cancellation");
    // Cleanup has returned, so a contained descendant is already dead; allow
    // only for the kernel closing its descriptors.
    let exited = fixture.descendant_exited(Duration::from_secs(5));
    if !exited {
        // Its FIFO descriptor is still open, so the pid has not been reused.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(alive, "{mode} descendant exited before cancellation");
    // The first child left the adapter's group and session; a double-forked
    // grandchild stays in that new session after its parent was reaped.
    assert_eq!((sid, pgid), (forked, forked));
    assert_eq!(pid == forked, mode == "setsid");
    assert!(
        matches!(
            result,
            Ok(RunResult::Cancelled {
                mode: CancelMode::Hard,
                forced: true
            })
        ),
        "{result:?}"
    );
    assert!(exited, "{mode} descendant {pid} survived hard cancellation");
}

#[test]
fn hard_cancel_stops_setsid_descendant() {
    hard_cancel_reaps_escaped("setsid");
}

#[test]
fn hard_cancel_stops_forked_chain_in_new_session() {
    hard_cancel_reaps_escaped("setsid_fork");
}

/// Linux only: the adapter is a child subreaper, so the orphaned grandchild
/// stays below it. macOS reparents it to launchd, outside any parent chain the
/// runtime can follow; docs/architecture/runtime.md states that limit.
#[cfg(target_os = "linux")]
#[test]
fn hard_cancel_stops_double_forked_descendant() {
    hard_cancel_reaps_escaped("double_fork");
}
