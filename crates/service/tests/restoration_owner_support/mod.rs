use foundry_contracts::{
    context::{ContextDigest, ContextFormat, ContextId, ContextPayload},
    context_v2::{LoadingScope, RestoreDescriptor, restore_binding},
};
use foundry_runtime::{AdapterSpec, coordinated::loading::*};
use foundry_service::{ServiceOptions, restoration_owner::*};
use foundry_store::{BeginRestoreInput, CheckpointRef, Store};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

// Reuse the existing neutral witness fixture builder; this suite tests the service.
#[path = "../../../store/tests/support_witness/mod.rs"]
mod witness;

pub struct Fixture {
    pub directory: tempfile::TempDir,
    pub path: PathBuf,
    pub session: String,
    pub head: CheckpointRef,
    pub options: RestoreOwnerOptions,
}
impl Fixture {
    pub fn new(lease: Duration) -> Self {
        Self::with_workspace(lease, None)
    }
    pub fn with_workspace(lease: Duration, workspace: Option<&std::path::Path>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.db");
        let mut store = Store::open(&path).unwrap();
        let session = match workspace {
            None => witness::checkpoint::session(&mut store),
            Some(path) => {
                store
                    .create(foundry_contracts::CreateSession {
                        prompt: "opaque storage fixture".into(),
                        directory: path.display().to_string(),
                        model: None,
                        idempotency_key: None,
                    })
                    .unwrap()
                    .id
            }
        };
        let fence = store.begin_attempt(&session, 0).unwrap();
        let input = witness::input(&store, &fence, "capture");
        let head = store
            .finish_with_witnessed_checkpoint(&fence, input, witness::terminal())
            .unwrap()
            .checkpoint;
        Self {
            directory,
            path,
            session,
            head,
            options: RestoreOwnerOptions {
                lease: ServiceOptions {
                    lease_duration: lease,
                    poll_interval: Duration::from_millis(25),
                },
                cleanup_timeout: Duration::from_secs(2),
            },
        }
    }
    pub fn start(&self, request: &str) -> RestorationOwner {
        let store = Store::open(&self.path).unwrap();
        RestorationOwner::start(
            self.path.clone(),
            self.session.clone(),
            BeginRestoreInput {
                request_id: request.into(),
                checkpoint_id: self.head.id.clone(),
                expected_session_version: store.get(&self.session).unwrap().version,
                configuration_digest: self.head.configuration_digest,
            },
            self.options,
        )
        .unwrap()
    }
    pub fn reserved(&self, owner: &mut RestorationOwner) -> LoadingScope {
        let expected = owner.pending_job().unwrap().clone();
        let reply = receive(owner);
        assert!(reply.job_id == expected);
        let RestoreReply::Reserved { scope, checkpoint } = reply.result.unwrap() else {
            panic!("reservation expected")
        };
        assert_eq!(*checkpoint, self.head);
        scope
    }
    pub fn load(&self, owner: &mut RestorationOwner, scope: LoadingScope) -> LoadedProcess {
        self.load_pollable(owner, scope, false).0
    }
    pub fn load_pollable(
        &self,
        owner: &mut RestorationOwner,
        scope: LoadingScope,
        altered: bool,
    ) -> (
        LoadedProcess,
        std::sync::mpsc::Receiver<foundry_contracts::CancelMode>,
    ) {
        owner.request_baseline().unwrap();
        let RestoreReply::Baseline(snapshot) = receive(owner).result.unwrap() else {
            panic!("baseline expected")
        };
        let payload = ContextPayload::new(snapshot.checkpoint().payload().to_vec()).unwrap();
        let id = |s| ContextId::new(s).unwrap();
        let descriptor = RestoreDescriptor {
            capture_id: id("capture"),
            checkpoint_id: id(self.head.id.as_str()),
            record_digest: ContextDigest::parse(&hex(&self.head.record_digest)).unwrap(),
            witness_digest: ContextDigest::parse(&hex(&snapshot.witness().digest())).unwrap(),
            format: ContextFormat {
                profile: id("fixture"),
                format_version: id("1"),
                producer_build: id("test"),
            },
            configuration_digest: ContextDigest::parse(&hex(&self.head.configuration_digest))
                .unwrap(),
            payload_length: payload.as_bytes().len() as u64,
            payload_digest: payload.digest(),
            retention_binding: ContextDigest::of(b"fixture"),
        };
        owner.bind_plan(descriptor).unwrap();
        let RestoreReply::PlanBound(descriptor) = receive(owner).result.unwrap() else {
            panic!("plan")
        };
        let mut descriptor = *descriptor;
        if altered {
            descriptor.retention_binding = ContextDigest::of(b"other plan");
        }
        let binding = restore_binding(&scope, &descriptor).unwrap().to_hex();
        let plan = LoadingPlan::new(
            self.directory.path().to_path_buf(),
            scope,
            descriptor,
            payload,
            LoadingLimits {
                phase_timeout: Duration::from_secs(3),
                retained_timeout: Duration::from_secs(20),
            },
            LoadingPurpose::Inspect,
            vec![],
        )
        .unwrap();
        let mut spec = AdapterSpec::new("/usr/bin/python3");
        spec.args = vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/restoration_owner_support/adapter.py")
                .into_os_string(),
            binding.into(),
        ];
        spec.cancel_timeout = Duration::from_millis(100);
        let cancelled = owner.take_cancellation().unwrap();
        let loaded = load(&spec, plan, &cancelled).unwrap();
        (loaded, cancelled)
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn receive(owner: &mut RestorationOwner) -> OwnerReply {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(reply) = owner.try_recv().unwrap() {
            return reply;
        }
        assert!(Instant::now() < deadline, "owner reply timed out");
        thread::sleep(Duration::from_millis(2));
    }
}
