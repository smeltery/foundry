use cap_std::fs::Dir;
use foundry_contracts::{CancelMode, Lifecycle, Session};
use foundry_runtime::{AdapterSpec, RunResult, RuntimeError, coordinated::*};
use foundry_tools::host;
use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::JoinHandle,
    time::Duration,
};

pub struct Fixture {
    pub root: PathBuf,
    python: PathBuf,
}
impl Fixture {
    pub fn new(helper: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("f5y-coordinator-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let output = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let python = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let source = format!(
            "#!{}\nmode={helper:?}\n{}",
            python.display(),
            include_str!("helper.py")
        );
        std::fs::write(root.join("helper"), source).unwrap();
        std::fs::set_permissions(root.join("helper"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        Self { root, python }
    }
    pub fn start(&self, adapter: &str, enabled: bool) -> Harness {
        self.start_with_capacity(adapter, enabled, PERSISTENCE_CAPACITY)
    }
    pub fn start_with_capacity(&self, adapter: &str, enabled: bool, capacity: usize) -> Harness {
        let mut spec = AdapterSpec::new(&self.python);
        spec.args = ["-u", "-c", include_str!("adapter.py"), adapter]
            .into_iter()
            .map(Into::into)
            .collect();
        spec.run_timeout = Duration::from_secs(4);
        spec.ready_timeout = Duration::from_secs(2);
        spec.cancel_timeout = Duration::from_millis(100);
        let session = Session {
            id: "session".into(),
            prompt: "test".into(),
            directory: self.root.to_string_lossy().into(),
            model: None,
            lifecycle: Lifecycle::Running,
            outcome: None,
            result: None,
            active_attempt_id: Some("attempt".into()),
            version: 1,
        };
        let policy = enabled.then(|| {
            TrustedToolPolicy::new(
                self.root.join("helper"),
                Dir::open_ambient_dir(&self.root, cap_std::ambient_authority()).unwrap(),
                vec![host::Grant::Read],
                host::HostLimits {
                    request_bytes: host::MAX_REQUEST_BYTES,
                    read_bytes: 262144,
                    write_bytes: 262144,
                    output_bytes: host::MAX_REPLY_BYTES,
                    entries: 256,
                },
                Duration::from_millis(500),
            )
            .unwrap()
        });
        let (requests, receive) = mpsc::sync_channel(capacity);
        let (replies, reply_rx) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
        let (cancel, cancellation) = mpsc::channel();
        let (result, results) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (_message, messages) = mpsc::channel();
            let outcome = run_adapter_coordinated(
                &spec,
                &session,
                "attempt",
                &cancellation,
                &messages,
                RuntimePersistence {
                    requests,
                    replies: reply_rx,
                },
                policy,
            );
            result.send(outcome).unwrap();
        });
        Harness {
            receive,
            replies,
            cancel,
            results,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub struct Harness {
    pub receive: Receiver<PersistenceRequest>,
    pub replies: SyncSender<PersistenceReply>,
    pub cancel: Sender<CancelMode>,
    pub results: Receiver<Result<RunResult, RuntimeError>>,
    worker: Option<JoinHandle<()>>,
}
impl Harness {
    pub fn next(&self) -> PersistenceRequest {
        self.receive.recv_timeout(Duration::from_secs(3)).unwrap()
    }
    pub fn event(&self) -> foundry_contracts::AdapterEvent {
        let PersistenceRequest::Event { checkpoint, event } = self.next() else {
            panic!("expected event")
        };
        self.replies
            .send(PersistenceReply::EventCommitted { checkpoint })
            .unwrap();
        event
    }
    pub fn claim(&self) -> CheckpointId {
        self.event();
        self.event();
        let PersistenceRequest::ClaimTool { checkpoint, intent } = self.next() else {
            panic!("expected claim")
        };
        assert_eq!(intent.call_id, "call");
        assert_eq!(intent.tool, "read");
        checkpoint
    }
    pub fn claimed(&self, checkpoint: CheckpointId) {
        self.replies
            .send(PersistenceReply::ToolClaimed { checkpoint })
            .unwrap();
    }
    pub fn finish(mut self) -> Result<RunResult, RuntimeError> {
        let result = self.results.recv_timeout(Duration::from_secs(5)).unwrap();
        self.worker.take().unwrap().join().unwrap();
        result
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cancel.send(CancelMode::Hard);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
