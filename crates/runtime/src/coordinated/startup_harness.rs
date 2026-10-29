use super::*;
use crate::{AdapterMessage, AdapterSpec, RunResult, coordinated::*};
use foundry_contracts::{AdapterEvent, CancelMode, Lifecycle};
use std::{path::PathBuf, process::Command, sync::mpsc, thread::JoinHandle};

pub(super) struct Harness {
    pub root: PathBuf,
    pub requests: mpsc::Receiver<PersistenceRequest>,
    pub replies: mpsc::SyncSender<PersistenceReply>,
    pub owner: CaptureOwnerEndpoint,
    pub cancel: mpsc::Sender<CancelMode>,
    pub messages: mpsc::Sender<AdapterMessage>,
    pub result: mpsc::Receiver<Result<engine::EngineExit, RuntimeError>>,
    worker: Option<JoinHandle<()>>,
}

impl Harness {
    pub fn new(mode: &str, timeout: Duration) -> Self {
        Self::with_helper(mode, timeout, None)
    }
    pub fn with_helper(mode: &str, timeout: Duration, helper: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!("f5y-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
        spec.args = [
            "-u",
            "-c",
            include_str!("startup_fixture.py"),
            root.to_str().unwrap(),
            mode,
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let policy = helper.map(|helper| {
            use foundry_tools::host;
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("helper");
            if helper != "missing" {
                let source = format!(
                    "#!{}\nmode={helper:?}\n{}",
                    spec.executable.display(),
                    include_str!("../../tests/coordinated/helper.py")
                );
                std::fs::write(&path, source).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            TrustedToolPolicy::new(
                path,
                cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap(),
                vec![host::Grant::Read],
                host::HostLimits {
                    request_bytes: host::MAX_REQUEST_BYTES,
                    read_bytes: 262144,
                    write_bytes: 262144,
                    output_bytes: host::MAX_REPLY_BYTES,
                    entries: 256,
                },
                Duration::from_secs(1),
            )
            .unwrap()
        });
        spec.run_timeout = Duration::from_secs(4);
        spec.cancel_timeout = Duration::from_millis(80);
        let session = Session {
            id: "s".into(),
            prompt: "fixture".into(),
            directory: root.to_string_lossy().into(),
            model: None,
            lifecycle: Lifecycle::Running,
            outcome: None,
            result: None,
            active_attempt_id: Some("attempt".into()),
            version: 1,
        };
        let (requests, receive) = mpsc::sync_channel(2);
        let (replies, reply_rx) = mpsc::sync_channel(2);
        let (owner, endpoint) = CaptureOwner::channel();
        let (cancel, cancellation) = mpsc::channel();
        let (messages, message_rx) = mpsc::channel();
        let (result_tx, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome = run_fresh_capture(
                &spec,
                &session,
                "attempt",
                &cancellation,
                &message_rx,
                RuntimePersistence {
                    requests,
                    replies: reply_rx,
                },
                FreshCaptureOptions {
                    profile: *profile(),
                    phase_timeout: timeout,
                    owner,
                    policy,
                },
            );
            let outcome = outcome.map(|exit| match exit {
                CaptureExit::ActionRequired(stop) => engine::EngineExit::ActionRequired(stop),
                CaptureExit::Prepared(prepared) => engine::EngineExit::Prepared(prepared),
                CaptureExit::Cancelled(stop) => engine::EngineExit::Cancelled(stop),
            });
            result_tx.send(outcome).unwrap();
        });
        Self {
            root,
            requests: receive,
            replies,
            owner: endpoint,
            cancel,
            messages,
            result,
            worker: Some(worker),
        }
    }
    pub fn event(&self) -> (CheckpointId, AdapterEvent) {
        let request = self
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|failure| {
                let detail = match self.result.try_recv() {
                    Ok(Err(error)) => format!("{error:?}"),
                    _ => "no runtime failure available".into(),
                };
                panic!("expected persistence event: {failure:?}; {detail}");
            });
        match request {
            PersistenceRequest::Event { checkpoint, event } => (checkpoint, event),
            _ => panic!("unexpected tool authority"),
        }
    }
    pub fn commit(&self, checkpoint: CheckpointId) {
        self.replies
            .send(PersistenceReply::EventCommitted { checkpoint })
            .unwrap();
    }
    pub fn ready(&self) {
        let (job, event) = self.event();
        assert!(matches!(event, AdapterEvent::Ready { .. }));
        self.commit(job);
    }
    pub fn activation(&self) -> (CaptureCorrelation, bool) {
        let CaptureRequest::ActivateAdmission {
            correlation,
            message_ack,
        } = self
            .owner
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!("expected activation")
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.commands().len() < 2 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        (correlation, message_ack)
    }
    pub fn activate(&self, correlation: CaptureCorrelation) {
        self.owner
            .replies
            .send(CaptureReply::AdmissionActivated { correlation })
            .ok()
            .unwrap();
    }
    pub fn commands(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("commands"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    pub fn finish(&mut self) -> Result<RunResult, RuntimeError> {
        match self.finish_owned()? {
            engine::EngineExit::Ordinary(result) => Ok(result),
            engine::EngineExit::Cancelled(stop) => Ok(RunResult::Cancelled {
                mode: stop.mode(),
                forced: stop.forced(),
            }),
            engine::EngineExit::ActionRequired(_) => {
                Err(RuntimeError::Invalid("private capture permission stop"))
            }
            engine::EngineExit::Prepared(_) => {
                Err(RuntimeError::Invalid("private capture prepared"))
            }
        }
    }
    pub fn finish_owned(&mut self) -> Result<engine::EngineExit, RuntimeError> {
        let result = self.result.recv_timeout(Duration::from_secs(2)).unwrap();
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
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

pub(super) fn profile() -> Box<ProfileBinding> {
    use crate::coordinated::capture::StoredFormat;
    Box::new(ProfileBinding {
        stored: StoredFormat {
            format_id: "stored-format".into(),
            format_version: 1,
            profile_id: "stored-profile".into(),
            binding_id: "binding".into(),
            binding_version: "1".into(),
        },
        wire: ContextFormat {
            profile: ContextId::new("wire-profile").unwrap(),
            format_version: ContextId::new("1").unwrap(),
            producer_build: ContextId::new("fixture").unwrap(),
        },
        configuration_digest: [7; 32],
    })
}
