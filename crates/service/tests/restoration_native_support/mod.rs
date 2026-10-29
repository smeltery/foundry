#![allow(dead_code)]
#[path = "../restoration_owner_support/mod.rs"]
mod support;
use foundry_contracts::{CancelMode, context::*, context_v2::*};
use foundry_runtime::{
    AdapterSpec,
    coordinated::{loading::continuation::ContinuationBinding, loading::*, origin::*},
};
use foundry_service::{native_validation::*, restoration_owner::*};
use foundry_store::{RestoreBaselineSnapshot, witness::LogicalRecord};
use std::{
    sync::{Arc, mpsc::Receiver},
    thread,
    time::{Duration, Instant},
};
pub use support::{Fixture, receive};

// Test-only comparator: validates a transport fixture, never real native history.
pub struct Comparator(pub u8);
impl NativeComparator for Comparator {
    fn compare(
        &self,
        job: &NativeValidationJob,
    ) -> Result<NativeComparison, NativeValidationFailure> {
        if self.0 == 4 {
            thread::sleep(Duration::from_millis(100));
        }
        if self.0 == 1 {
            return Err(NativeValidationFailure::Mapping);
        }
        if self.0 == 3 {
            while !job.cancelled() {
                thread::sleep(Duration::from_millis(2));
            }
            return Err(NativeValidationFailure::Cancelled);
        }
        if job.observation() != b"detached-native-observation-fixture"
            || job.expectations().policy != b"fixture-policy"
        {
            return Err(NativeValidationFailure::Invalid);
        }
        Ok(NativeComparison {
            job_id: job.binding().job_id.clone(),
            input_digest: if self.0 == 2 {
                ContextDigest::of(b"wrong")
            } else {
                job.input_digest()
            },
            registration_digest: job.registration_digest(),
            instance_id: job.instance_id().clone(),
            observation_digest: ContextDigest::of(job.observation()),
            profile_digest: job.binding().profile_digest,
            configuration_digest: job.expectations().configuration_digest,
            graph_digest: ContextDigest::of(b"fixture-graph"),
            mappings_digest: ContextDigest::of(b"fixture-mappings"),
        })
    }
}
pub fn digest(bytes: [u8; 32]) -> ContextDigest {
    ContextDigest::parse(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap()
}
fn origin(snapshot: &RestoreBaselineSnapshot, profile: ContextDigest) -> OriginIdentity {
    let head = snapshot.checkpoint().reference();
    let witness = snapshot.witness();
    let body = witness.body();
    let LogicalRecord::Prompt(prompt) = &body.records[0] else {
        panic!()
    };
    let id = |value: &str| ContextId::new(value).unwrap();
    OriginIdentity::restored(RestoredBaselineIdentity {
        session_id: id(&head.session_id),
        checkpoint_id: id(head.id.as_str()),
        generation: head.generation,
        source_attempt_id: id(&head.source_attempt_id),
        original_prompt_attempt_id: id(&prompt.attempt_id),
        prompt_digest: digest(prompt.text_digest),
        prompt_bytes: prompt.text_bytes,
        record_digest: digest(head.record_digest),
        payload_digest: digest(head.payload_digest),
        witness_digest: digest(witness.digest()),
        witness_bytes: body.encode().unwrap().bytes().len() as u64,
        history_binding: ContextDigest::of(b"fixture-history"),
        prefix_binding: ContextDigest::of(b"fixture-prefix"),
        logical_records: body.records.len() as u64,
        prefix_records: body.prefix.len() as u64,
        mapping_refs: body.mappings.len() as u64,
        profile_binding: profile,
    })
    .unwrap()
}
pub struct Harness {
    pub fixture: Fixture,
    pub owner: RestorationOwner,
    pub loaded: LoadedProcess,
    pub cancelled: Receiver<CancelMode>,
    pub binding: ContinuationBinding,
}
impl Harness {
    pub fn new(comparator: u8, mode: &str) -> Self {
        let fixture = Fixture::new(Duration::from_secs(3));
        let mut owner = fixture.start("load");
        let scope = fixture.reserved(&mut owner);
        owner.request_baseline().unwrap();
        let RestoreReply::Baseline(snapshot) = receive(&mut owner).result.unwrap() else {
            panic!()
        };
        let payload = ContextPayload::new(snapshot.checkpoint().payload().to_vec()).unwrap();
        let id = |value: &str| ContextId::new(value).unwrap();
        let descriptor = RestoreDescriptor {
            capture_id: id("capture"),
            checkpoint_id: id(fixture.head.id.as_str()),
            record_digest: digest(fixture.head.record_digest),
            witness_digest: digest(snapshot.witness().digest()),
            format: ContextFormat {
                profile: id("fixture"),
                format_version: id("1"),
                producer_build: id("test"),
            },
            configuration_digest: digest(fixture.head.configuration_digest),
            payload_length: payload.as_bytes().len() as u64,
            payload_digest: payload.digest(),
            retention_binding: ContextDigest::of(b"fixture"),
        };
        let begin = restore_binding(&scope, &descriptor).unwrap();
        owner.bind_plan(descriptor).unwrap();
        let RestoreReply::PlanBound(descriptor) = receive(&mut owner).result.unwrap() else {
            panic!()
        };
        let profile = ContextDigest::of(b"fixture-profile");
        let baseline = origin(&snapshot, profile);
        let baseline_copy = OriginIdentity::decode(&baseline.encode().unwrap()).unwrap();
        owner
            .prepare_native(
                NativeValidatorRegistration {
                    identity: ContextDigest::of(b"fixture-registration"),
                    timeout: Duration::from_millis(300),
                    comparator: Arc::new(Comparator(comparator)),
                },
                NativeExpectations {
                    profile_digest: profile,
                    configuration_digest: ContextDigest::of(b"effective-fixture-policy"),
                    policy: b"fixture-policy".to_vec(),
                    associations: b"fixture-mappings".to_vec(),
                    origin: baseline_copy,
                },
            )
            .unwrap();
        let RestoreReply::NativePrepared(observation) = receive(&mut owner).result.unwrap() else {
            panic!()
        };
        let plan = LoadingPlan::new(
            fixture.directory.path().to_path_buf(),
            scope,
            *descriptor,
            payload,
            LoadingLimits {
                phase_timeout: Duration::from_secs(2),
                retained_timeout: Duration::from_secs(10),
            },
            LoadingPurpose::PrepareContinuation,
            vec![],
        )
        .unwrap();
        let mut spec = AdapterSpec::new("/usr/bin/python3");
        spec.args = vec![
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../runtime/src/coordinated/loading/fixture.py")
                .into_os_string(),
            mode.into(),
            begin.to_hex().into(),
            fixture.directory.path().join("wire.jsonl").into_os_string(),
        ];
        spec.cancel_timeout = Duration::from_millis(100);
        let cancelled = owner.take_cancellation().unwrap();
        let mut loaded = load(&spec, plan, &cancelled).unwrap();
        owner.register_loaded(&loaded).unwrap();
        receive(&mut owner).result.unwrap();
        loaded
            .begin_observation(observation.clone(), &cancelled)
            .unwrap();
        let frame = loop {
            if let Some(frame) = loaded.poll_observation(&cancelled).unwrap() {
                break frame;
            }
            thread::sleep(Duration::from_millis(2));
        };
        let instance_id = frame.instance_id().unwrap().clone();
        owner.validate_native(&loaded, frame).unwrap();
        receive(&mut owner).result.unwrap();
        Self {
            fixture,
            owner,
            loaded,
            cancelled,
            binding: ContinuationBinding {
                effective_configuration_digest: ContextDigest::of(b"effective-fixture-policy"),
                observation,
                instance_id,
                baseline,
            },
        }
    }
    pub fn validated(&mut self) -> ContextDigest {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.loaded.poll(&self.cancelled).unwrap();
            self.owner.native_status().unwrap();
            match receive(&mut self.owner).result.unwrap() {
                RestoreReply::NativeValidated(binding) => return binding,
                RestoreReply::NativePending => {}
                _ => panic!(),
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    }
}
