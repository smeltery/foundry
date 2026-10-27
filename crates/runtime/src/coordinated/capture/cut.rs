use super::{
    CaptureError,
    bounds::{self, Encoding},
};
use foundry_contracts::context::{ContextDigest, ContextFormat};
use serde::Serialize;

pub const MAX_CUT_BYTES: usize = 96 * 1024;
pub struct CoreCutFacts {
    pub work_revision: u64,
    pub event_seq: u64,
    pub event_digest: [u8; 32],
    pub input_digest: [u8; 32],
    pub tool_digest: [u8; 32],
}
pub struct HeadIdentity {
    pub checkpoint_id: String,
    pub generation: u64,
}
pub struct CoreCutIdentity {
    pub session_id: String,
    pub attempt_id: String,
    pub request_id: String,
    pub expected_head: Option<HeadIdentity>,
}
pub struct StoredFormat {
    pub format_id: String,
    pub format_version: u32,
    pub profile_id: String,
    pub binding_id: String,
    pub binding_version: String,
}
pub struct DurableCoverage {
    pub message_ids: Vec<String>,
    pub tool_call_ids: Vec<String>,
}
pub struct ProfileBinding {
    pub stored: StoredFormat,
    pub wire: ContextFormat,
    pub configuration_digest: [u8; 32],
}
impl ProfileBinding {
    pub fn validate(&self) -> Result<(), CaptureError> {
        for value in [
            &self.stored.format_id,
            &self.stored.profile_id,
            &self.stored.binding_id,
            &self.stored.binding_version,
        ] {
            bounds::identifier(value)?;
        }
        if self.stored.format_version == 0 {
            return Err(CaptureError::Invalid);
        }
        Ok(())
    }
    pub fn wire_digest(&self) -> Result<ContextDigest, CaptureError> {
        self.validate()?;
        let mut bytes = Encoding::new(b"foundry.capture.wire-format.v1", 16384)?;
        for value in [
            &self.wire.profile,
            &self.wire.format_version,
            &self.wire.producer_build,
        ] {
            bytes.field(value.as_str().as_bytes())?;
        }
        Ok(bytes.digest())
    }
    /// Convert the existing digest bytes, never hash the digest again.
    pub fn configuration_wire_digest(&self) -> Result<ContextDigest, CaptureError> {
        let hex: String = self
            .configuration_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        ContextDigest::parse(&hex).map_err(|_| CaptureError::Invalid)
    }
}

/// Owner-supplied durable facts. Construction checks bounds, not database/native truth.
pub struct CaptureCut {
    identity: CoreCutIdentity,
    facts: CoreCutFacts,
    profile: ProfileBinding,
    coverage: DurableCoverage,
    digest: ContextDigest,
}

impl CaptureCut {
    pub fn from_trusted_owner(
        identity: CoreCutIdentity,
        facts: CoreCutFacts,
        profile: ProfileBinding,
        coverage: DurableCoverage,
    ) -> Result<Self, CaptureError> {
        for id in [
            &identity.session_id,
            &identity.attempt_id,
            &identity.request_id,
        ] {
            bounds::identifier(id)?;
        }
        if facts.work_revision > i64::MAX as u64 || facts.event_seq > i64::MAX as u64 {
            return Err(CaptureError::Invalid);
        }
        profile.validate()?;
        if let Some(head) = &identity.expected_head {
            bounds::identifier(&head.checkpoint_id)?;
            let uuid =
                uuid::Uuid::parse_str(&head.checkpoint_id).map_err(|_| CaptureError::Invalid)?;
            if uuid.to_string() != head.checkpoint_id
                || head.generation == 0
                || head.generation > i64::MAX as u64
            {
                return Err(CaptureError::Invalid);
            }
        }
        for ids in [&coverage.message_ids, &coverage.tool_call_ids] {
            if ids.len() > 1024 {
                return Err(CaptureError::Limit);
            }
            for id in ids {
                bounds::identifier(id)?;
            }
            if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(CaptureError::Invalid);
            }
        }
        #[derive(Serialize)]
        struct Coverage<'a> {
            message_ids: &'a [String],
            tool_call_ids: &'a [String],
        }
        bounds::measure(
            &Coverage {
                message_ids: &coverage.message_ids,
                tool_call_ids: &coverage.tool_call_ids,
            },
            65536,
        )?;
        let digest = binding(&identity, &facts, &profile, &coverage)?;
        Ok(Self {
            identity,
            facts,
            profile,
            coverage,
            digest,
        })
    }
    pub fn identity(&self) -> &CoreCutIdentity {
        &self.identity
    }
    pub fn facts(&self) -> &CoreCutFacts {
        &self.facts
    }
    pub fn profile(&self) -> &ProfileBinding {
        &self.profile
    }
    pub fn coverage(&self) -> &DurableCoverage {
        &self.coverage
    }
    pub fn binding_digest(&self) -> ContextDigest {
        self.digest
    }
    pub fn into_owner_parts(
        self,
    ) -> (
        CoreCutIdentity,
        CoreCutFacts,
        ProfileBinding,
        DurableCoverage,
    ) {
        (self.identity, self.facts, self.profile, self.coverage)
    }
}

fn binding(
    identity: &CoreCutIdentity,
    facts: &CoreCutFacts,
    profile: &ProfileBinding,
    coverage: &DurableCoverage,
) -> Result<ContextDigest, CaptureError> {
    let mut bytes = Encoding::new(b"foundry.capture.cut.v1", MAX_CUT_BYTES)?;
    for value in [
        &identity.session_id,
        &identity.attempt_id,
        &identity.request_id,
    ] {
        bytes.field(value.as_bytes())?;
    }
    if let Some(head) = &identity.expected_head {
        bytes.field(&[1])?;
        bytes.field(head.checkpoint_id.as_bytes())?;
        bytes.field(&head.generation.to_be_bytes())?;
    } else {
        bytes.field(&[0])?;
    }
    bytes.field(&facts.work_revision.to_be_bytes())?;
    bytes.field(&facts.event_seq.to_be_bytes())?;
    for digest in [&facts.event_digest, &facts.input_digest, &facts.tool_digest] {
        bytes.field(digest)?;
    }
    let format = &profile.stored;
    for value in [
        &format.format_id,
        &format.profile_id,
        &format.binding_id,
        &format.binding_version,
    ] {
        bytes.field(value.as_bytes())?;
    }
    bytes.field(&format.format_version.to_be_bytes())?;
    for value in [
        &profile.wire.profile,
        &profile.wire.format_version,
        &profile.wire.producer_build,
    ] {
        bytes.field(value.as_str().as_bytes())?;
    }
    bytes.field(&profile.configuration_digest)?;
    for ids in [&coverage.message_ids, &coverage.tool_call_ids] {
        bytes.field(&(ids.len() as u64).to_be_bytes())?;
        for id in ids {
            bytes.field(id.as_bytes())?;
        }
    }
    Ok(bytes.digest())
}
