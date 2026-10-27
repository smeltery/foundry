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
time.sleep(10)
"#,
    );
    fixture.detached = Some(fixture.wait_file("detached").parse().unwrap());
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
    // The detached process is deliberately outside the owned group. Its live
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
