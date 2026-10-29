//! Registered native publication comparisons over actor-owned durable evidence.
use crate::native_validation::NativeValidationFailure;
use foundry_contracts::context::ContextDigest;
use foundry_runtime::coordinated::{
    RuntimePersistence,
    capture::{CaptureValidationJob, OwnerValidationAttestation, ProfileBinding},
    capture_owner::CaptureOwner,
};
use foundry_store::{
    CaptureSnapshot, ContinueAcceptanceRef, RestoreBaselineSnapshot, witness::WitnessBodyV1,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Registered trusted code, executed off the actor/runtime threads. Native syntax
/// remains in the external host. The host must enforce bounded work/child cleanup.
pub trait NativePublicationComparator: Send + Sync + 'static {
    fn compare(
        &self,
        job: &NativePublicationJob,
    ) -> Result<NativePublicationComparison, NativeValidationFailure>;
}
pub struct NativePublicationRegistration {
    pub identity: ContextDigest,
    pub timeout: Duration,
    pub comparator: Arc<dyn NativePublicationComparator>,
}

/// Immutable expected profile and producer evidence. Select these independently of
/// candidate bytes; a trusted bridge may own their capture under the explicit TCB.
pub struct PublicationConfiguration {
    pub profile: ProfileBinding,
    pub policy: Vec<u8>,
    /// Retained associations OR fixed trusted-producer configuration. For live
    /// capture the registered host resolves its completed producer record after
    /// settlement; never derive the producer/path/expected mappings from candidate.
    pub associations: Vec<u8>,
    pub validator: NativePublicationRegistration,
}

/// No public constructor, credential getter, Debug or Serialize.
pub struct NativePublicationJob {
    pub(crate) runtime: Box<CaptureValidationJob>,
    pub(crate) snapshot: Arc<CaptureSnapshot>,
    pub(crate) events: Arc<[foundry_store::RestoreEventEvidence]>,
    pub(crate) baseline: Option<Arc<RestoreBaselineSnapshot>>,
    pub(crate) acceptance: Option<ContinueAcceptanceRef>,
    pub(crate) registration: ContextDigest,
    pub(crate) input_digest: ContextDigest,
    pub(crate) policy: Arc<[u8]>,
    pub(crate) associations: Arc<[u8]>,
    pub(crate) deadline: Instant,
    pub(crate) cancelled: Arc<AtomicBool>,
}
impl NativePublicationJob {
    pub fn events(&self) -> &[foundry_store::RestoreEventEvidence] {
        &self.events
    }
    pub fn runtime_job(&self) -> &CaptureValidationJob {
        &self.runtime
    }
    pub fn snapshot(&self) -> &CaptureSnapshot {
        &self.snapshot
    }
    pub fn baseline(&self) -> Option<&RestoreBaselineSnapshot> {
        self.baseline.as_deref()
    }
    pub fn acceptance(&self) -> Option<&ContinueAcceptanceRef> {
        self.acceptance.as_ref()
    }
    pub fn registration_digest(&self) -> ContextDigest {
        self.registration
    }
    pub fn input_digest(&self) -> ContextDigest {
        self.input_digest
    }
    pub fn policy(&self) -> &[u8] {
        &self.policy
    }
    pub fn associations(&self) -> &[u8] {
        &self.associations
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
}

/// Returned only after the registered comparator checks actual native semantics and
/// independent associations. Store additionally verifies the full witness and cut.
pub struct NativePublicationComparison {
    pub input_digest: ContextDigest,
    pub registration_digest: ContextDigest,
    pub candidate_digest: ContextDigest,
    pub attestation: OwnerValidationAttestation,
    pub witness: WitnessBodyV1,
}

/// Runtime endpoints and metadata; never an AttemptFence or publication approval.
pub struct CapturePorts {
    pub messages: std::sync::mpsc::Receiver<foundry_runtime::AdapterMessage>,
    pub session: foundry_contracts::Session,
    pub attempt_id: String,
    pub profile: ProfileBinding,
    pub owner: CaptureOwner,
    pub persistence: RuntimePersistence,
}
