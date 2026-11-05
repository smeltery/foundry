#[path = "helper_install.rs"]
mod helper_install;
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

// Ordering is asserted through persistence requests and replies, never elapsed time.
// These bounds only detect a stuck run, so they sit far above process startup on a
// loaded workstation and passing tests never wait for them.
/// Upper bound for one persistence request or the run's final result.
pub const WAIT: Duration = Duration::from_secs(20);
const RUN_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);
/// Only the hang helper reaches its deadline. The deadline starts before spawn, so it
/// must still exceed the helper's spawn, exec and interpreter startup.
const HANG_HELPER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Fixture {
    pub root: PathBuf,
    python: PathBuf,
    helper: PathBuf,
    helper_timeout: Duration,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!("f5y-coordinator-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let output = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let python = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        std::fs::write(root.join("helper-mode"), mode).unwrap();
        let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
        let helper = helper_install::installed_helper(directory, &python);
        let helper_timeout = if mode == "hang" {
            HANG_HELPER_TIMEOUT
        } else {
            HELPER_TIMEOUT
        };
        Self {
            root,
            python,
            helper,
            helper_timeout,
        }
    }
    pub fn start(&self, adapter: &str, enabled: bool) -> Harness {
        self.start_with_capacity(adapter, enabled, PERSISTENCE_CAPACITY)
    }
    pub fn start_with_capacity(&self, adapter: &str, enabled: bool, capacity: usize) -> Harness {
        self.start_configured(adapter, enabled, capacity, false, vec![host::Grant::Read])
    }
    pub fn start_permissions(&self, grants: Vec<host::Grant>) -> Harness {
        self.start_configured("normal", true, PERSISTENCE_CAPACITY, true, grants)
    }
    fn start_configured(
        &self,
        adapter: &str,
        enabled: bool,
        capacity: usize,
        required: bool,
        grants: Vec<host::Grant>,
    ) -> Harness {
        let mut spec = AdapterSpec::new(&self.python);
        spec.args = ["-u", "-c", include_str!("adapter.py"), adapter]
            .into_iter()
            .map(Into::into)
            .collect();
        spec.run_timeout = RUN_TIMEOUT;
        spec.ready_timeout = READY_TIMEOUT;
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
            metadata: Default::default(),
        };
        let policy = enabled.then(|| {
            TrustedToolPolicy::new(
                self.helper.clone(),
                Dir::open_ambient_dir(&self.root, cap_std::ambient_authority()).unwrap(),
                grants,
                host::HostLimits {
                    request_bytes: host::MAX_REQUEST_BYTES,
                    read_bytes: 262144,
                    write_bytes: 262144,
                    output_bytes: host::MAX_REPLY_BYTES,
                    entries: 256,
                },
                self.helper_timeout,
            )
            .unwrap()
        });
        let policy = policy.map(|policy| {
            if required {
                policy.require_authorization()
            } else {
                policy
            }
        });
        let identity = policy.as_ref().map(TrustedToolPolicy::identity);
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
            identity,
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
    pub identity: Option<ToolPolicyIdentity>,
    pub receive: Receiver<PersistenceRequest>,
    pub replies: SyncSender<PersistenceReply>,
    pub cancel: Sender<CancelMode>,
    pub results: Receiver<Result<RunResult, RuntimeError>>,
    worker: Option<JoinHandle<()>>,
}
impl Harness {
    pub fn next(&self) -> PersistenceRequest {
        match self.receive.recv_timeout(WAIT) {
            Ok(request) => request,
            Err(error) => panic!(
                "expected persistence request: {error:?}, run outcome {:?}",
                self.results.try_recv()
            ),
        }
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
        let result = self.results.recv_timeout(WAIT).unwrap();
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
