use super::super::tests::{Fixture, plan};
use super::*;
use crate::coordinated::origin::RestoredBaselineIdentity;
use std::{sync::mpsc, thread};

pub(super) struct Harness {
    pub(super) fixture: Fixture,
    pub(super) pending: PendingContinuation,
    pub(super) requests: Receiver<ContinuationRequest>,
    pub(super) replies: SyncSender<ContinuationReply>,
    pub(super) cancel: mpsc::Sender<CancelMode>,
    pub(super) cancellation: Receiver<CancelMode>,
}
pub(super) fn loaded(
    mode: &str,
    purpose: LoadingPurpose,
) -> (
    Fixture,
    LoadedProcess,
    ContinuationBinding,
    mpsc::Sender<CancelMode>,
    Receiver<CancelMode>,
) {
    let mut plan = plan();
    plan.purpose = purpose;
    let mut descriptor: RestoreDescriptor =
        serde_json::from_value(serde_json::to_value(plan.begin.descriptor().unwrap()).unwrap())
            .unwrap();
    let checkpoint_id = ContextId::new(uuid::Uuid::new_v4().to_string()).unwrap();
    descriptor.checkpoint_id = checkpoint_id.clone();
    plan.binding = context_v2::restore_binding(&plan.scope, &descriptor).unwrap();
    plan.begin = LoadingFrame::begin(plan.scope.clone(), descriptor).unwrap();
    let fixture = Fixture::new(mode, &plan);
    let (cancel, cancellation) = mpsc::channel();
    let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
    let observation = ObservationBinding {
        scope: loaded.scope.clone(),
        begin_binding: loaded.binding,
        job_id: ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        profile_digest: ContextDigest::of(b"profile"),
    };
    loaded
        .begin_observation(observation.clone(), &cancellation)
        .unwrap();
    let frame = loop {
        if let Some(f) = loaded.poll_observation(&cancellation).unwrap() {
            break f;
        }
        thread::sleep(Duration::from_millis(2));
    };
    let id = |s| ContextId::new(s).unwrap();
    let baseline = OriginIdentity::restored(RestoredBaselineIdentity {
        session_id: id("session"),
        checkpoint_id,
        generation: 1,
        source_attempt_id: id("old"),
        original_prompt_attempt_id: id("original"),
        prompt_digest: ContextDigest::of(b"historical"),
        prompt_bytes: 10,
        record_digest: ContextDigest::of(b"record"),
        payload_digest: ContextDigest::of(b"private-context"),
        witness_digest: ContextDigest::of(b"witness"),
        witness_bytes: 100,
        history_binding: ContextDigest::of(b"history"),
        prefix_binding: ContextDigest::of(b"prefix"),
        logical_records: 1,
        prefix_records: 2,
        mapping_refs: 1,
        profile_binding: observation.profile_digest,
    })
    .unwrap();
    let binding = ContinuationBinding {
        observation,
        instance_id: frame.instance_id().unwrap().clone(),
        baseline,
        effective_configuration_digest: ContextDigest::of(b"effective-config"),
    };
    (fixture, loaded, binding, cancel, cancellation)
}
impl Harness {
    pub(super) fn new(mode: &str) -> Self {
        let (fixture, loaded, binding, cancel, cancellation) =
            loaded(mode, LoadingPurpose::PrepareContinuation);
        let (tx, requests) = mpsc::sync_channel(2);
        let (replies, rx) = mpsc::sync_channel(2);
        let pending = loaded
            .continue_with(
                binding,
                ContinuationOwner {
                    requests: tx,
                    replies: rx,
                },
                &cancellation,
            )
            .unwrap();
        Self {
            fixture,
            pending,
            requests,
            replies,
            cancel,
            cancellation,
        }
    }
    pub(super) fn grant(&mut self) -> (ContinuationJob, ContextDigest) {
        assert!(!self.pending.poll(&self.cancellation).unwrap());
        let ContinuationRequest::Authorize {
            job,
            tail_receipt_digest,
            binding,
        } = self.requests.recv().unwrap()
        else {
            panic!()
        };
        assert!(binding.effective_configuration_digest == ContextDigest::of(b"effective-config"));
        let id = |v| ContextId::new(v).unwrap();
        self.replies
            .send(ContinuationReply::Granted {
                job: job.clone(),
                identity: ContinuationIdentity {
                    attempt_id: id("new"),
                    continuation_id: id("continuation"),
                    input_id: id("input"),
                    input_sequence: 1,
                    input_digest: ContextDigest::of(b"new instruction"),
                    input_bytes: 15,
                    authorization_binding: ContextDigest::of(b"owner-authorized"),
                },
                text: "new instruction".into(),
            })
            .ok()
            .unwrap();
        (job, tail_receipt_digest)
    }
    pub(super) fn accepted(&mut self, digest: ContextDigest) -> ContinuationJob {
        loop {
            assert!(!self.pending.poll(&self.cancellation).unwrap());
            if let Ok(ContinuationRequest::Accept {
                job,
                receipt,
                identity,
                ..
            }) = self.requests.try_recv()
            {
                assert!(ContextDigest::of(receipt.as_str().as_bytes()) == digest);
                assert_eq!(identity.attempt_id.as_str(), "new");
                return job;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
