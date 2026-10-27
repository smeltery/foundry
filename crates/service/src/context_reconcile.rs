//! Reconcile one settled helper receipt; native inclusion is a separate check.
use crate::{Result, ServiceError, tool_digest};
use foundry_contracts::context::ContextDigest;
use foundry_runtime::coordinated::capture::{ResultProvenance, TraceEntry, TraceKind};
use foundry_store::{CaptureToolEvidence, ToolOutcome};
use foundry_tools::host::{self, Effect, Input, Outcome, Reply};

/// Inputs must come from the owned bridge and the retained atomic snapshot.
/// This checks one helper pair, not complete coverage, grants or native semantics.
pub(crate) fn reconcile_helper(
    record: &CaptureToolEvidence,
    input: &Input,
    reply: &Reply,
    delivered: &TraceEntry,
) -> Result<()> {
    let TraceKind::ToolResult(ResultProvenance::PersistedHelper {
        ledger_output_digest,
    }) = delivered.kind()
    else {
        return Err(mismatch());
    };
    if reply.version != host::VERSION
        || reply.attempt_id != record.attempt_id
        || reply.call_id != record.call_id
        || delivered.attempt_id() != record.attempt_id
        || delivered.id() != record.call_id
        || !matches!(
            (record.tool.as_str(), input),
            ("read" | "list", Input::Path(_)) | ("write", Input::Write(_))
        )
    {
        return Err(mismatch());
    }
    bounded(input, host::MAX_REQUEST_BYTES)?;
    bounded(reply, host::MAX_REPLY_BYTES)?;
    if tool_digest::input(&record.tool, input)? != record.input_digest {
        return Err(mismatch());
    }
    let outcome = match &reply.result {
        Outcome::Ok { .. } => ToolOutcome::Success,
        Outcome::Error {
            effect: Effect::None,
            ..
        } => ToolOutcome::Failed,
        Outcome::Error {
            effect: Effect::Unknown,
            ..
        } => return Err(mismatch()),
    };
    let digest = tool_digest::output(reply)?;
    if outcome != record.outcome
        || record.output_digest != Some(digest)
        || *ledger_output_digest != digest
        || ContextDigest::of(delivered.bytes()) != delivered.wire_digest()
    {
        return Err(mismatch());
    }
    // The current typed helper path enqueues this exact encoding. Other bridge
    // transformations require their own explicit binding, never hash substitution.
    let body = serde_json::to_vec(&reply.result).map_err(|_| mismatch())?;
    if body != delivered.bytes() {
        return Err(mismatch());
    }
    Ok(())
}

fn bounded(value: &impl serde::Serialize, limit: usize) -> Result<()> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).map_err(|_| mismatch())
}

fn mismatch() -> ServiceError {
    ServiceError::Coordination("helper capture evidence mismatch")
}
