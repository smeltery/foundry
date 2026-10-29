//! Detached live-state evidence, never native approval or permission to continue.
use crate::{
    MAX_FRAME_BYTES,
    context::{ContextDigest, ContextError, ContextPayload, ContextReceipt},
    context_v2::LoadingScope,
};
use serde::{Deserialize, Serialize};

pub const OBSERVE_CAPABILITY: &str = "context_observe_v1";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationBinding {
    pub scope: LoadingScope,
    pub job_id: ContextReceipt,
    pub begin_binding: ContextDigest,
    pub profile_digest: ContextDigest,
}

/// Fixed diagnostics only; rejected observations cannot authorize execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationFailure {
    Unsupported,
    InvalidState,
    Limit,
    Validation,
    Cancelled,
}

/// Private framing. Correlation does not authenticate the producer or its claims.
/// ```compile_fail
/// use foundry_contracts::context_observation::ObservationFrame;
/// fn log(frame: &ObservationFrame) { println!("{frame:?}"); }
/// ```
/// ```compile_fail
/// use foundry_contracts::context_observation::ObservationFrame;
/// fn persist(frame: &ObservationFrame) { serde_json::to_vec(frame).unwrap(); }
/// ```
pub struct ObservationFrame(Wire);

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Wire {
    #[serde(rename = "context_observe_v1")]
    Observe {
        binding: ObservationBinding,
        challenge: ContextReceipt,
    },
    #[serde(rename = "context_observed_v1")]
    Observed {
        binding: ObservationBinding,
        instance_id: ContextReceipt,
        payload_length: u64,
        payload_digest: ContextDigest,
        payload_base64: String,
        challenge: ContextReceipt,
    },
    #[serde(rename = "context_observation_rejected_v1")]
    Rejected {
        binding: ObservationBinding,
        code: ObservationFailure,
        challenge: ContextReceipt,
    },
}

impl ObservationFrame {
    pub fn request(binding: ObservationBinding, challenge: ContextReceipt) -> Self {
        Self(Wire::Observe { binding, challenge })
    }

    /// The payload must be a detached observation of the retained live instance.
    /// This codec checks bytes and bounds, not the truth of that assertion.
    pub fn observed(
        binding: ObservationBinding,
        instance_id: ContextReceipt,
        payload: ContextPayload,
        challenge: ContextReceipt,
    ) -> Self {
        Self(Wire::Observed {
            binding,
            instance_id,
            payload_length: payload.as_bytes().len() as u64,
            payload_digest: payload.digest(),
            payload_base64: payload.encoded(),
            challenge,
        })
    }

    pub fn rejected(
        binding: ObservationBinding,
        code: ObservationFailure,
        challenge: ContextReceipt,
    ) -> Self {
        Self(Wire::Rejected {
            binding,
            code,
            challenge,
        })
    }

    pub fn binding(&self) -> &ObservationBinding {
        match &self.0 {
            Wire::Observe { binding, .. }
            | Wire::Observed { binding, .. }
            | Wire::Rejected { binding, .. } => binding,
        }
    }

    pub fn challenge(&self) -> &ContextReceipt {
        match &self.0 {
            Wire::Observe { challenge, .. }
            | Wire::Observed { challenge, .. }
            | Wire::Rejected { challenge, .. } => challenge,
        }
    }

    pub fn instance_id(&self) -> Option<&ContextReceipt> {
        match &self.0 {
            Wire::Observed { instance_id, .. } => Some(instance_id),
            _ => None,
        }
    }

    pub fn failure(&self) -> Option<ObservationFailure> {
        match &self.0 {
            Wire::Rejected { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Pure response comparison. A rejection still passes correlation, not validation.
    pub fn check_response(
        &self,
        binding: &ObservationBinding,
        challenge: &ContextReceipt,
    ) -> Result<(), ContextError> {
        if matches!(self.0, Wire::Observe { .. })
            || self.binding() != binding
            || self.challenge() != challenge
        {
            return Err(ContextError::Integrity);
        }
        Ok(())
    }

    pub fn payload(&self) -> Result<Option<ContextPayload>, ContextError> {
        match &self.0 {
            Wire::Observed {
                payload_length,
                payload_digest,
                payload_base64,
                ..
            } => {
                let payload = ContextPayload::decode(payload_base64)?;
                if payload.as_bytes().len() as u64 != *payload_length
                    || payload.digest() != *payload_digest
                {
                    return Err(ContextError::Integrity);
                }
                Ok(Some(payload))
            }
            _ => Ok(None),
        }
    }
}

pub fn encode(frame: &ObservationFrame) -> Result<Vec<u8>, ContextError> {
    // Every field is bounded: validated identifiers/digests and at most 1 MiB body.
    frame.payload()?;
    let bytes = serde_json::to_vec(&frame.0).map_err(|_| ContextError::Schema)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ContextError::Limit);
    }
    Ok(bytes)
}

pub fn decode_request(bytes: &[u8]) -> Result<ObservationFrame, ContextError> {
    decode(bytes, true)
}

pub fn decode_response(bytes: &[u8]) -> Result<ObservationFrame, ContextError> {
    decode(bytes, false)
}

fn decode(bytes: &[u8], request: bool) -> Result<ObservationFrame, ContextError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ContextError::Limit);
    }
    let frame = ObservationFrame(serde_json::from_slice(bytes).map_err(|_| ContextError::Schema)?);
    if matches!(frame.0, Wire::Observe { .. }) != request {
        return Err(ContextError::Frame);
    }
    frame.payload()?;
    Ok(frame)
}
