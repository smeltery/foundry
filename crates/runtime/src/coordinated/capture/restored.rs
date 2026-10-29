//! Exact historical baseline and accepted continuation, separate from new delivery.
use super::*;
use crate::coordinated::loading::continuation::ContinuationBinding;
use foundry_contracts::context_v2::ContinuationIdentity;
use std::sync::Arc;

/// Metadata only. The service must validate current durable and native facts.
pub struct RestoredCaptureOrigin {
    pub binding: Arc<ContinuationBinding>,
    pub continuation: ContinuationIdentity,
    pub historical_configuration_digest: foundry_contracts::context::ContextDigest,
}
impl RestoredCaptureOrigin {
    pub(crate) fn check_cut(&self, cut: &CaptureCut) -> Result<(), CaptureError> {
        let baseline = self
            .binding
            .baseline
            .restored_identity()
            .ok_or(CaptureError::Binding)?;
        let head = cut
            .identity()
            .expected_head
            .as_ref()
            .ok_or(CaptureError::Binding)?;
        if head.checkpoint_id != baseline.checkpoint_id.as_str()
            || head.generation != baseline.generation
            || cut.identity().session_id != baseline.session_id.as_str()
            || cut.identity().attempt_id != self.continuation.attempt_id.as_str()
            || cut.profile().configuration_wire_digest()?
                != self.binding.effective_configuration_digest
        {
            return Err(CaptureError::Binding);
        }
        Ok(())
    }
    pub(crate) fn check_trace(&self, trace: &DeliveredTrace) -> Result<(), CaptureError> {
        let first = trace.entries().first().ok_or(CaptureError::Pending)?;
        if !matches!(first.kind(), TraceKind::Instruction)
            || first.id() != self.continuation.input_id.as_str()
            || first.wire_digest() != self.continuation.input_digest
            || first.bytes().len() as u64 != self.continuation.input_bytes
            || trace.entries().iter().any(|entry| {
                entry.attempt_id() != self.continuation.attempt_id.as_str()
                    || matches!(entry.kind(), TraceKind::OpeningPrompt)
            })
        {
            return Err(CaptureError::Binding);
        }
        Ok(())
    }
    pub(super) fn encode_binding(
        &self,
        binding: &mut bounds::Encoding,
    ) -> Result<(), CaptureError> {
        binding.field(self.binding.baseline.binding_digest()?.to_hex().as_bytes())?;
        binding.field(&bounds::json(&self.binding.observation, 4096)?)?;
        binding.field(self.binding.instance_id.as_str().as_bytes())?;
        binding.field(self.historical_configuration_digest.to_hex().as_bytes())?;
        binding.field(
            self.binding
                .effective_configuration_digest
                .to_hex()
                .as_bytes(),
        )?;
        binding.field(&bounds::json(&self.continuation, 4096)?)?;
        Ok(())
    }
}
