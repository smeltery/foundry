//! Convert a cleaned-up runtime handoff without publishing or inventing validation.
use crate::{Result, ServiceError, context_cut::CaptureEvidence};
use foundry_contracts::context::ContextDigest;
use foundry_runtime::coordinated::{CleanupReceipt, PreparedCapture};
use foundry_store::{
    AttemptFence, CheckpointCut, ContextCheckpointId, ContextFormat, CoverageManifest,
    OpaqueCheckpointInput, TerminalProposal,
};
use serde_json::json;

/// Private pending bytes, not a durable checkpoint or completed session.
pub(crate) struct PendingFinalization {
    input: OpaqueCheckpointInput,
    terminal: TerminalProposal,
    request_digest: [u8; 32],
    cleanup: CleanupReceipt,
}
impl PendingFinalization {
    pub(crate) fn request_id(&self) -> &str {
        &self.input.request_id
    }
    pub(crate) fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }
    pub(crate) fn into_parts(self) -> (OpaqueCheckpointInput, TerminalProposal, CleanupReceipt) {
        (self.input, self.terminal, self.cleanup)
    }
}

/// The expected binding must come from the independently cached validated owner job.
/// This function neither performs native validation nor checks current lease ownership.
pub(crate) fn prepare_finalization(
    prepared: PreparedCapture,
    fence: &AttemptFence,
    evidence: &CaptureEvidence,
    expected_validation_binding: ContextDigest,
) -> Result<PendingFinalization> {
    let verified = prepared.verified();
    evidence.check(fence, verified.cut())?;
    if verified.binding_digest() != expected_validation_binding {
        return Err(ServiceError::Coordination(
            "capture validation binding mismatch",
        ));
    }
    // Inspect bindings before consuming the only private payload/cleanup handoff.
    let (verified, cleanup) = prepared.into_owner_parts();
    let (cut, payload, attestation, terminal) = verified.into_owner_parts();
    let (identity, facts, profile, coverage) = cut.into_owner_parts();
    let (lifecycle, outcome, result) = terminal.into_parts();
    let terminal = TerminalProposal {
        lifecycle,
        outcome: Some(outcome),
        result: Some(result),
    };
    let annotation = json!({
        "schema_version": 1,
        "wire_format": {
            "profile": profile.wire.profile.as_str(),
            "format_version": profile.wire.format_version.as_str(),
            "producer_build": profile.wire.producer_build.as_str(),
        },
        "validator_id": attestation.validator_id.as_str(),
        "validator_version": attestation.validator_version.as_str(),
        "completed_turn_id": attestation.completed_turn_id.as_str(),
        "validation_binding": expected_validation_binding.to_hex(),
    });
    let input = OpaqueCheckpointInput {
        request_id: identity.request_id,
        expected_head: identity
            .expected_head
            .map(|head| ContextCheckpointId::parse(&head.checkpoint_id))
            .transpose()?,
        cut: CheckpointCut {
            work_revision: facts.work_revision,
            event_seq: facts.event_seq,
            event_digest: facts.event_digest,
            input_digest: facts.input_digest,
            tool_digest: facts.tool_digest,
        },
        format: ContextFormat {
            format_id: profile.stored.format_id,
            format_version: profile.stored.format_version,
            profile_id: profile.stored.profile_id,
            binding_id: profile.stored.binding_id,
            binding_version: profile.stored.binding_version,
        },
        configuration_digest: profile.configuration_digest,
        coverage: CoverageManifest {
            message_ids: coverage.message_ids,
            tool_call_ids: coverage.tool_call_ids,
        },
        payload: payload.into_bytes(),
        producer_validation: Some(annotation),
    };
    // Store owns canonical encoding and the combined annotation/format/terminal limit.
    let request_digest = input.finalization_request_digest(fence, &terminal)?;
    Ok(PendingFinalization {
        input,
        terminal,
        request_digest,
        cleanup,
    })
}
