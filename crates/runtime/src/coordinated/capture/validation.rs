use super::{
    CaptureCut, CaptureError, CaptureTerminal, DeliveredTrace, RestoredCaptureOrigin,
    bounds::{self, Encoding},
};
use crate::coordinated::capture_owner::CaptureCorrelation;
use foundry_contracts::context::*;
use std::sync::Arc;

pub struct CaptureCandidate {
    format: ContextFormat,
    coverage: ContextCoverage,
    payload: ContextPayload,
}
impl CaptureCandidate {
    pub fn format(&self) -> &ContextFormat {
        &self.format
    }
    pub fn coverage(&self) -> &ContextCoverage {
        &self.coverage
    }
    pub fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }
}

pub struct OwnerValidationAttestation {
    pub validator_id: ContextId,
    pub validator_version: ContextId,
    pub completed_turn_id: ContextId,
}

/// Holding this job is not proof of native semantics. Only the trusted owner validates.
pub struct CaptureValidationJob {
    correlation: CaptureCorrelation,
    cut: CaptureCut,
    candidate: CaptureCandidate,
    trace: DeliveredTrace,
    binding: ContextDigest,
    terminal: CaptureTerminal,
    restored: Option<Arc<RestoredCaptureOrigin>>,
}

impl CaptureValidationJob {
    #[cfg(test)]
    pub(crate) fn new(
        correlation: &CaptureCorrelation,
        cut: CaptureCut,
        event: ContextEvent,
        challenge: &ContextReceipt,
        trace: DeliveredTrace,
        terminal: CaptureTerminal,
    ) -> Result<Self, CaptureError> {
        Self::with_origin(correlation, cut, event, challenge, trace, terminal, None)
    }
    pub(crate) fn with_origin(
        correlation: &CaptureCorrelation,
        cut: CaptureCut,
        event: ContextEvent,
        challenge: &ContextReceipt,
        trace: DeliveredTrace,
        terminal: CaptureTerminal,
        restored: Option<Arc<RestoredCaptureOrigin>>,
    ) -> Result<Self, CaptureError> {
        let ContextEvent::Candidate {
            attempt_id,
            capture_id,
            challenge: echoed,
            format,
            coverage,
            payload,
        } = event
        else {
            return Err(CaptureError::Invalid);
        };
        if attempt_id != correlation.attempt_id
            || capture_id != correlation.capture_id
            || &echoed != challenge
            || cut.identity().attempt_id != attempt_id.as_str()
            || !same_format(&format, &cut.profile().wire)
        {
            return Err(CaptureError::Binding);
        }
        if !trace.is_quiescent() {
            return Err(CaptureError::Pending);
        }
        if let Some(origin) = &restored {
            origin.check_cut(&cut)?;
            origin.check_trace(&trace)?;
        } else {
            let opening = &trace.entries()[0];
            if !matches!(opening.kind(), super::TraceKind::OpeningPrompt)
                || opening.id() != cut.identity().session_id
                || opening.attempt_id() != attempt_id.as_str()
            {
                return Err(CaptureError::Binding);
            }
        }
        if coverage.inputs.len() > 1024
            || coverage.results.len() > 1024
            || coverage
                .inputs
                .iter()
                .any(|entry| entry.native_ids.is_empty() || entry.native_ids.len() > 1024)
            || coverage
                .results
                .iter()
                .any(|entry| entry.native_ids.is_empty() || entry.native_ids.len() > 1024)
        {
            return Err(CaptureError::Limit);
        }
        let metadata = bounds::json(&coverage, MAX_COVERAGE_BYTES)?;
        let domain: &[u8] = if restored.is_some() {
            b"foundry.capture.validation.v3"
        } else {
            b"foundry.capture.validation.v2"
        };
        let mut binding = Encoding::new(domain, if restored.is_some() { 16384 } else { 4096 })?;
        binding.field(correlation.job.0.as_bytes())?;
        binding.field(correlation.attempt_id.as_str().as_bytes())?;
        binding.field(correlation.capture_id.as_str().as_bytes())?;
        binding.field(cut.binding_digest().to_hex().as_bytes())?;
        binding.field(payload.digest().to_hex().as_bytes())?;
        binding.field(&(payload.as_bytes().len() as u64).to_be_bytes())?;
        binding.field(ContextDigest::of(&metadata).to_hex().as_bytes())?;
        binding.field(trace.digest()?.to_hex().as_bytes())?;
        binding.field(terminal.binding_digest().to_hex().as_bytes())?;
        if let Some(origin) = &restored {
            origin.encode_binding(&mut binding)?;
        }
        Ok(Self {
            correlation: correlation.clone(),
            cut,
            candidate: CaptureCandidate {
                format,
                coverage,
                payload,
            },
            trace,
            binding: binding.digest(),
            terminal,
            restored,
        })
    }
    /// Exact runtime-generated correlation checked against the candidate at construction.
    pub fn correlation(&self) -> &CaptureCorrelation {
        &self.correlation
    }
    pub fn restored_origin(&self) -> Option<&RestoredCaptureOrigin> {
        self.restored.as_deref()
    }
    pub fn cut(&self) -> &CaptureCut {
        &self.cut
    }
    pub fn candidate(&self) -> &CaptureCandidate {
        &self.candidate
    }
    pub fn trace(&self) -> &DeliveredTrace {
        &self.trace
    }
    pub fn terminal(&self) -> &CaptureTerminal {
        &self.terminal
    }
    pub fn binding_digest(&self) -> ContextDigest {
        self.binding
    }

    /// Consuming assertion by the trusted linked owner, NOT compiler/native proof.
    /// The owner must run its reviewed semantic validator before calling this.
    pub fn accept_from_trusted_owner(
        self,
        attestation: OwnerValidationAttestation,
    ) -> Result<VerifiedCapture, CaptureError> {
        if attestation.completed_turn_id != self.candidate.coverage.completed_turn_id {
            return Err(CaptureError::Binding);
        }
        Ok(VerifiedCapture {
            cut: self.cut,
            payload: self.candidate.payload,
            attestation,
            binding: self.binding,
            terminal: self.terminal,
            restored: self.restored,
        })
    }
}

/// Trusted-owner attestation over exact bytes, not a codec/digest proof of semantics.
/// ```compile_fail
/// use foundry_runtime::coordinated::capture::VerifiedCapture;
/// fn log(value: &VerifiedCapture) { println!("{value:?}"); }
/// ```
/// ```compile_fail
/// use foundry_runtime::coordinated::capture::VerifiedCapture;
/// fn persist(value: &VerifiedCapture) { serde_json::to_string(value).unwrap(); }
/// ```
pub struct VerifiedCapture {
    cut: CaptureCut,
    payload: ContextPayload,
    attestation: OwnerValidationAttestation,
    binding: ContextDigest,
    terminal: CaptureTerminal,
    restored: Option<Arc<RestoredCaptureOrigin>>,
}
impl VerifiedCapture {
    pub fn restored_origin(&self) -> Option<&RestoredCaptureOrigin> {
        self.restored.as_deref()
    }
    pub fn cut(&self) -> &CaptureCut {
        &self.cut
    }
    pub fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }
    pub fn terminal(&self) -> &CaptureTerminal {
        &self.terminal
    }
    pub fn binding_digest(&self) -> ContextDigest {
        self.binding
    }
    pub fn into_owner_parts(
        self,
    ) -> (
        CaptureCut,
        ContextPayload,
        OwnerValidationAttestation,
        CaptureTerminal,
    ) {
        (self.cut, self.payload, self.attestation, self.terminal)
    }
}

fn same_format(left: &ContextFormat, right: &ContextFormat) -> bool {
    left.profile == right.profile
        && left.format_version == right.format_version
        && left.producer_build == right.producer_build
}
