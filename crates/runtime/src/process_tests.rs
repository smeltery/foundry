use super::*;
use foundry_contracts::Lifecycle;
use std::time::{Duration, Instant};

struct Fixture {
    directory: std::path::PathBuf,
    detached: Option<i32>,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("f5y-workers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        Self {
            directory,
            detached: None,
        }
    }
    fn spawn(&self, source: &str) -> Process {
        let python = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
        spec.args = ["-u", "-c", source].into_iter().map(Into::into).collect();
        Process::spawn(
            &spec,
            &Session {
                id: "s".into(),
                prompt: "fixture".into(),
                directory: self.directory.to_string_lossy().into(),
                model: None,
                lifecycle: Lifecycle::Running,
                outcome: None,
                result: None,
                active_attempt_id: Some("a".into()),
                version: 1,
                metadata: Default::default(),
            },
        )
        .unwrap()
    }
    fn wait_file(&self, name: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(value) = std::fs::read_to_string(self.directory.join(name))
                && !value.is_empty()
            {
                return value;
            }
            assert!(Instant::now() < deadline, "fixture did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = self.detached {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn detached_pipe_holder_does_not_prevent_owned_workers_joining() {
    let mut fixture = Fixture::new();
    let mut process = fixture.spawn(
        r#"
import os,time,pathlib,fcntl
assert not (fcntl.fcntl(0,fcntl.F_GETFL) & os.O_NONBLOCK)
assert not (fcntl.fcntl(1,fcntl.F_GETFL) & os.O_NONBLOCK)
assert not (fcntl.fcntl(2,fcntl.F_GETFL) & os.O_NONBLOCK)
pid=os.fork()
if pid==0:
    os.setsid()
    pathlib.Path('detached').write_text(str(os.getpid()))
    time.sleep(10)
    os._exit(0)
"#,
    );
    fixture.detached = Some(fixture.wait_file("detached").parse().unwrap());
    // The leader exits right after forking, orphaning the holder outside any
    // parent chain that cleanup follows. Keep the leader unreaped meanwhile.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            process.child.id(),
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0);
    process
        .send(&AdapterCommand::Message {
            attempt_id: "a".into(),
            message_id: "m".into(),
            text: "x".repeat(262144),
            receipt: "r".into(),
        })
        .unwrap();
    assert_eq!(process.workers.remaining(), 3);
    let start = Instant::now();
    process.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(process.workers.remaining(), 0);
    assert!(process.commands.is_none());
    assert!(process.child.try_wait().unwrap().is_some());
    // The detached process is deliberately beyond cleanup's reach. Its live
    // descriptors cannot hold our joins hostage; the fixture cleans it up.
    assert_eq!(unsafe { libc::kill(fixture.detached.unwrap(), 0) }, 0);
}

#[test]
fn full_event_queue_is_closed_and_joined_without_a_consumer() {
    let fixture = Fixture::new();
    let mut process = fixture.spawn(
        r#"
import json,pathlib,time
for i in range(64):
    print(json.dumps({'type':'text','attempt_id':'a','text':'x'}),flush=True)
pathlib.Path('written').write_text('done')
time.sleep(10)
"#,
    );
    fixture.wait_file("written");
    thread::sleep(Duration::from_millis(50));
    assert_eq!(process.workers.remaining(), 3);
    let start = Instant::now();
    process.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(process.workers.remaining(), 0);
    assert!(process.child.try_wait().unwrap().is_some());
    process.stop().unwrap();
}

/// Spawns an adapter that exits at once and leaves it an unreaped zombie.
fn exited_adapter(fixture: &Fixture) -> Process {
    let process = fixture.spawn("");
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            process.child.id(),
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0);
    process
}

#[test]
fn exited_adapter_waits_out_group_teardown_lag_but_not_a_live_member() {
    // macOS can keep reporting a reaped zombie's group with EPERM before ESRCH.
    let fixture = Fixture::new();
    let mut process = exited_adapter(&fixture);
    let mut probes = [libc::EPERM, libc::EPERM, libc::ESRCH]
        .into_iter()
        .map(Some);
    assert!(
        process
            .reap_exiting_group(|| probes.next().unwrap())
            .unwrap()
    );
    assert!(process.stopped);
    process.stop().unwrap();
    // A signalable member after the reap means the group is not proven gone.
    let mut process = exited_adapter(&fixture);
    let mut probes = [Some(libc::EPERM), None].into_iter();
    assert!(
        !process
            .reap_exiting_group(|| probes.next().unwrap())
            .unwrap()
    );
}

#[test]
fn exited_adapter_group_stop_reaches_the_reap_probe() {
    // Only checks that stop runs the EPERM, reap and probe path with the real signal
    // probe; whether macOS lags here depends on load, so it cannot prove lag handling.
    let fixture = Fixture::new();
    for _ in 0..3 {
        let mut process = exited_adapter(&fixture);
        process.stop().unwrap();
        assert!(process.child.try_wait().unwrap().is_some());
    }
}
