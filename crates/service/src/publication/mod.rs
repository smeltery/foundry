use crate::{
    Result, ServiceError,
    context_cut::{CaptureEvidence, prepare_cut},
    native_publication::*,
    tool_worker::{PersistenceState, WorkerControl},
};
use foundry_contracts::context::{ContextDigest, ContextId};
use foundry_runtime::coordinated::{
    PERSISTENCE_CAPACITY, PersistenceReply, PersistenceRequest, PreparedCapture,
    RuntimePersistence,
    capture::{ProfileBinding, StoredFormat},
    capture_owner::*,
};
use foundry_store::{
    AttemptFence, ContinueAcceptanceRef, FinalizedCheckpoint, RestoreBaselineSnapshot, Store,
    WitnessedCheckpointInput,
};
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};
mod validation;

pub(crate) struct Publication {
    configuration: PublicationConfiguration,
    profile: Option<ProfileBinding>,
    capture: CaptureOwnerEndpoint,
    requests: mpsc::Receiver<PersistenceRequest>,
    replies: mpsc::SyncSender<PersistenceReply>,
    persistence: PersistenceState,
    control: WorkerControl,
    baseline: Option<Arc<RestoreBaselineSnapshot>>,
    acceptance: Option<ContinueAcceptanceRef>,
    capture_id: Option<ContextId>,
    jobs: Vec<CaptureJobId>,
    stage: u8,
    evidence: Option<CaptureEvidence>,
    validation: Option<validation::Task>,
    verified: Option<(ContextDigest, foundry_store::witness::WitnessBodyV1)>,
}
impl Publication {
    pub fn new(
        store: &Store,
        fence: &AttemptFence,
        configuration: PublicationConfiguration,
        control: WorkerControl,
        baseline: Option<Arc<RestoreBaselineSnapshot>>,
        acceptance: Option<ContinueAcceptanceRef>,
    ) -> Result<(Self, CapturePorts)> {
        configuration
            .profile
            .validate()
            .map_err(|_| ServiceError::Invalid("invalid capture profile"))?;
        if configuration.policy.len() > 65536
            || configuration.associations.len() > 262144
            || configuration.validator.timeout.is_zero()
            || Instant::now()
                .checked_add(configuration.validator.timeout)
                .is_none()
            || baseline.is_some() != acceptance.is_some()
        {
            return Err(ServiceError::Invalid("invalid publication registration"));
        }
        let (owner, capture) = CaptureOwner::channel();
        let (requests_tx, requests) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
        let (replies, replies_rx) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
        let ports = CapturePorts {
            session: store.get(&fence.session_id)?,
            attempt_id: fence.attempt_id.clone(),
            profile: copy_profile(&configuration.profile),
            owner,
            persistence: RuntimePersistence {
                requests: requests_tx,
                replies: replies_rx,
            },
        };
        let profile = Some(copy_profile(&configuration.profile));
        Ok((
            Self {
                configuration,
                profile,
                capture,
                requests,
                replies,
                persistence: PersistenceState::default(),
                control,
                baseline,
                acceptance,
                capture_id: None,
                jobs: Vec::new(),
                stage: 0,
                evidence: None,
                validation: None,
                verified: None,
            },
            ports,
        ))
    }
    pub fn poll(&mut self, store: &mut Store, fence: &AttemptFence) -> Result<()> {
        self.live(store, fence)?;
        match self.requests.try_recv() {
            Ok(request) => {
                self.persistence
                    .handle(store, fence, request, &self.replies, &self.control)?
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
        }
        if let Some(task) = self.validation.as_mut()
            && let Some((correlation, job, compared)) = task.poll()?
        {
            self.live(store, fence)?;
            self.evidence
                .as_ref()
                .ok_or(ServiceError::Coordination("capture cut absent"))?
                .check(fence, job.runtime.cut())?;
            if store.capture_snapshot(fence)? != *job.snapshot {
                return Err(ServiceError::Coordination("publication cut changed"));
            }
            compared.witness.encode()?;
            if compared.witness.header.validation_binding
                != super::restoration_owner::validation::raw_digest(job.runtime.binding_digest())
            {
                return Err(ServiceError::Coordination(
                    "publication witness validation binding mismatch",
                ));
            }
            self.live(store, fence)?;
            let binding = job.runtime.binding_digest();
            let verified = job
                .runtime
                .accept_from_trusted_owner(compared.attestation)
                .map_err(|_| ServiceError::Coordination("publication attestation mismatch"))?;
            self.verified = Some((binding, compared.witness));
            self.validation = None;
            self.reply(CaptureReply::CandidateVerified {
                correlation,
                verified: Box::new(verified),
            })?;
        }
        let request = match self.capture.requests.try_recv() {
            Ok(request) => request,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(()),
        };
        let correlation = match &request {
            CaptureRequest::ActivateAdmission { correlation, .. }
            | CaptureRequest::FreezeAdmission { correlation }
            | CaptureRequest::PrepareCut { correlation }
            | CaptureRequest::ValidateCandidate { correlation, .. } => correlation.clone(),
        };
        if correlation.attempt_id.as_str() != fence.attempt_id
            || self.jobs.contains(&correlation.job)
            || self.jobs.len() >= 4
            || self
                .capture_id
                .as_ref()
                .is_some_and(|id| id != &correlation.capture_id)
        {
            return Err(ServiceError::Coordination(
                "capture owner correlation mismatch",
            ));
        }
        self.capture_id = Some(correlation.capture_id.clone());
        self.jobs.push(correlation.job);
        let reply = match request {
            CaptureRequest::ActivateAdmission { correlation, .. }
                if self.stage == 0 && self.baseline.is_none() =>
            {
                self.stage = 1;
                // This owner does not introduce another message delivery loop.
                CaptureReply::AdmissionActivated { correlation }
            }
            CaptureRequest::FreezeAdmission { correlation }
                if self.stage <= 1 && self.persistence.idle() =>
            {
                self.control.signals.stop_admission();
                self.stage = 2;
                CaptureReply::AdmissionFrozen { correlation }
            }
            CaptureRequest::PrepareCut { correlation } if self.stage == 2 => {
                let profile = self.profile.take().ok_or(ServiceError::Coordination(
                    "capture profile already consumed",
                ))?;
                let (cut, evidence) = prepare_cut(
                    store,
                    fence,
                    correlation.capture_id.as_str().into(),
                    profile,
                )?;
                self.evidence = Some(evidence);
                self.stage = 3;
                CaptureReply::CutPrepared {
                    correlation,
                    cut: Box::new(cut),
                }
            }
            CaptureRequest::ValidateCandidate { correlation, job } if self.stage == 3 => {
                let evidence = self
                    .evidence
                    .as_ref()
                    .ok_or(ServiceError::Coordination("capture evidence absent"))?;
                evidence.check(fence, job.cut())?;
                self.validation = Some(validation::Task::start(
                    &self.configuration,
                    correlation,
                    job,
                    Arc::new(evidence.snapshot().clone()),
                    self.baseline.clone(),
                    self.acceptance.clone(),
                )?);
                self.stage = 4;
                return Ok(());
            }
            _ => return Err(ServiceError::Coordination("capture owner phase mismatch")),
        };
        self.live(store, fence)?;
        self.reply(reply)
    }
    pub fn check_prepared(&self, fence: &AttemptFence, prepared: &PreparedCapture) -> Result<()> {
        let (binding, _) = self
            .verified
            .as_ref()
            .ok_or(ServiceError::Coordination("publication validation absent"))?;
        let evidence = self
            .evidence
            .as_ref()
            .ok_or(ServiceError::Coordination("publication cut absent"))?;
        evidence.check(fence, prepared.verified().cut())?;
        if prepared.verified().binding_digest() != *binding
            || prepared.verified().cut().binding_digest() != evidence.cut_binding()
        {
            return Err(ServiceError::Coordination(
                "publication cleanup correlation mismatch",
            ));
        }
        Ok(())
    }
    pub fn finish(
        &mut self,
        store: &mut Store,
        fence: &AttemptFence,
        prepared: PreparedCapture,
    ) -> Result<FinalizedCheckpoint> {
        self.live(store, fence)?;
        self.check_prepared(fence, &prepared)?;
        let (binding, witness) = self.verified.take().ok_or(ServiceError::Coordination(
            "registered publication validation absent",
        ))?;
        let evidence = self
            .evidence
            .as_ref()
            .ok_or(ServiceError::Coordination("capture evidence absent"))?;
        let pending =
            crate::context_commit::prepare_finalization(prepared, fence, evidence, binding)?;
        let opaque_request = pending.request_id().to_owned();
        let opaque_digest = pending.request_digest();
        let (checkpoint, terminal, _cleanup) = pending.into_parts();
        if checkpoint.request_id != opaque_request
            || checkpoint.finalization_request_digest(fence, &terminal)? != opaque_digest
        {
            return Err(ServiceError::Coordination("prepared finalization changed"));
        }
        let input = WitnessedCheckpointInput {
            checkpoint,
            witness,
        };
        let request = input.checkpoint.request_id.clone();
        let digest = input.finalization_request_digest(fence, &terminal)?;
        // Store rechecks fence, cancellation, exact current cut/head and all witness facts atomically.
        match store.finish_with_witnessed_checkpoint(fence, input, terminal) {
            Ok(finalized) => Ok(finalized),
            Err(error) => {
                match store.finished_checkpoint_request(&fence.session_id, &request, digest)? {
                    Some(finalized) => Ok(finalized),
                    None => Err(error.into()),
                }
            }
        }
    }
    fn live(&self, store: &Store, fence: &AttemptFence) -> Result<()> {
        if self
            .control
            .expired
            .load(std::sync::atomic::Ordering::Acquire)
            || self
                .control
                .signals
                .relay(&self.control.cancellation)
                .is_some()
            || store.cancellation(fence)?.is_some()
        {
            return Err(ServiceError::Coordination(
                "capture owner cancelled or expired",
            ));
        }
        Ok(())
    }
    fn reply(&self, reply: CaptureReply) -> Result<()> {
        self.capture
            .replies
            .try_send(reply)
            .map_err(|_| ServiceError::Coordination("capture owner reply lost"))
    }
    pub fn cancel(&self) {
        if let Some(task) = &self.validation {
            task.cancel();
        }
    }
}
fn copy_profile(profile: &ProfileBinding) -> ProfileBinding {
    ProfileBinding {
        stored: StoredFormat {
            format_id: profile.stored.format_id.clone(),
            format_version: profile.stored.format_version,
            profile_id: profile.stored.profile_id.clone(),
            binding_id: profile.stored.binding_id.clone(),
            binding_version: profile.stored.binding_version.clone(),
        },
        wire: profile.wire.clone(),
        configuration_digest: profile.configuration_digest,
    }
}
