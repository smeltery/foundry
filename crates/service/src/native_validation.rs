//! Explicitly registered native comparators. Implementations are trusted code.
//! Candidate bytes and successful return values alone do not confer Store authority.
use foundry_contracts::{
    context::{ContextDigest, ContextPayload, ContextReceipt},
    context_observation::ObservationBinding,
};
use foundry_runtime::coordinated::origin::OriginIdentity;
use foundry_store::RestoreBaselineSnapshot;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeValidationFailure {
    Unsupported,
    Invalid,
    Policy,
    Mapping,
    Cancelled,
    Deadline,
    Cleanup,
    Internal,
}

/// Called on a dedicated worker, never the SQL actor or runtime poll thread.
/// A pure implementation must be bounded and perform no blocking I/O. A host
/// process implementation must enforce its own deadline and checked process cleanup.
pub trait NativeComparator: Send + Sync + 'static {
    fn compare(
        &self,
        job: &NativeValidationJob,
    ) -> Result<NativeComparison, NativeValidationFailure>;
}

/// Trusted application composition, not deserializable candidate configuration.
pub struct NativeValidatorRegistration {
    pub identity: ContextDigest,
    pub timeout: Duration,
    pub comparator: Arc<dyn NativeComparator>,
}

/// Selected before observation; exact native vocabulary belongs to the host codec.
/// Policy and associations use registered versioned codecs, never candidate schemas.
pub struct NativeExpectations {
    pub profile_digest: ContextDigest,
    /// Effective native policy, distinct from the stored selection configuration.
    pub configuration_digest: ContextDigest,
    pub policy: Vec<u8>,
    pub associations: Vec<u8>,
    pub origin: OriginIdentity,
}

/// Actor-constructed immutable inputs. No public constructor, Debug or Serialize.
pub struct NativeValidationJob {
    pub(crate) binding: ObservationBinding,
    pub(crate) registration: ContextDigest,
    pub(crate) snapshot: Arc<RestoreBaselineSnapshot>,
    pub(crate) expected: Arc<NativeExpectations>,
    pub(crate) challenge: ContextReceipt,
    pub(crate) instance: ContextReceipt,
    pub(crate) observation: ContextPayload,
    pub(crate) input_digest: ContextDigest,
    pub(crate) deadline: Instant,
    pub(crate) cancelled: Arc<AtomicBool>,
}
impl NativeValidationJob {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
    pub fn binding(&self) -> &ObservationBinding {
        &self.binding
    }
    pub fn registration_digest(&self) -> ContextDigest {
        self.registration
    }
    pub fn snapshot(&self) -> &RestoreBaselineSnapshot {
        &self.snapshot
    }
    pub fn expectations(&self) -> &NativeExpectations {
        &self.expected
    }
    pub fn challenge(&self) -> &ContextReceipt {
        &self.challenge
    }
    pub fn instance_id(&self) -> &ContextReceipt {
        &self.instance
    }
    pub fn observation(&self) -> &[u8] {
        self.observation.as_bytes()
    }
    pub fn input_digest(&self) -> ContextDigest {
        self.input_digest
    }
}

/// A registered comparator's correlated evidence, never a public approval token.
/// Commitments describe the exact graph/mapping checked by the registered codec.
pub struct NativeComparison {
    pub job_id: ContextReceipt,
    pub input_digest: ContextDigest,
    pub registration_digest: ContextDigest,
    pub instance_id: ContextReceipt,
    pub observation_digest: ContextDigest,
    pub profile_digest: ContextDigest,
    pub configuration_digest: ContextDigest,
    pub graph_digest: ContextDigest,
    pub mappings_digest: ContextDigest,
}
