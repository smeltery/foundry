use super::{
    CaptureError,
    bounds::{self, Encoding},
};
use crate::coordinated::CheckpointId;
use foundry_contracts::{MAX_PROMPT_BYTES, context::ContextDigest};
use serde_json::Value;

pub const MAX_TRACE_BYTES: usize = 1_048_576;
pub const MAX_TRACE_ENTRIES: usize = 1024;
/// Conservative charge for fixed digests, discriminants and acknowledgment identity.
pub const TRACE_ENTRY_FIXED_BYTES: usize = 128;

pub enum ResultProvenance {
    PersistedHelper { ledger_output_digest: [u8; 32] },
    SyntheticNoEffect,
    Denied,
}
pub enum TraceKind {
    OpeningPrompt,
    Instruction,
    ToolResult(ResultProvenance),
}
enum Acknowledgment {
    NotRequired,
    Submitted,
    Receipt(CheckpointId),
    Durable,
}

pub struct TraceEntry {
    kind: TraceKind,
    attempt: String,
    id: String,
    body: Vec<u8>,
    digest: ContextDigest,
    acknowledgment: Acknowledgment,
}
impl TraceEntry {
    pub fn kind(&self) -> &TraceKind {
        &self.kind
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }
    /// Exact body digest; never the service's domain-separated full helper Reply digest.
    pub fn wire_digest(&self) -> ContextDigest {
        self.digest
    }
    pub fn instruction_is_durable(&self) -> bool {
        matches!(self.acknowledgment, Acknowledgment::Durable)
    }
}

pub struct DeliveredTrace {
    entries: Vec<TraceEntry>,
    used: usize,
    restored: bool,
}
impl Default for DeliveredTrace {
    fn default() -> Self {
        Self::new()
    }
}
impl DeliveredTrace {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            used: 0,
            restored: false,
        }
    }
    pub(crate) fn restored_instruction(
        identity: &foundry_contracts::context_v2::ContinuationIdentity,
        text: &str,
    ) -> Result<Self, CaptureError> {
        let mut trace = Self::new();
        trace.restored = true;
        trace
            .reserve_instruction(
                identity.attempt_id.as_str(),
                identity.input_id.as_str(),
                text,
            )?
            .commit_enqueued();
        Ok(trace)
    }
    pub(crate) fn confirm_continuation(&mut self) {
        self.entries[0].acknowledgment = Acknowledgment::Durable;
    }
    pub fn entries(&self) -> &[TraceEntry] {
        &self.entries
    }
    pub fn used_bytes(&self) -> usize {
        self.used
    }
    pub fn is_quiescent(&self) -> bool {
        !self.entries.is_empty()
            && self.entries.iter().all(|entry| {
                matches!(
                    entry.acknowledgment,
                    Acknowledgment::NotRequired | Acknowledgment::Durable
                )
            })
    }
    fn capacity(&self, attempt: &str, id: &str, body_bytes: usize) -> Result<usize, CaptureError> {
        bounds::identifier(attempt)?;
        bounds::identifier(id)?;
        if self
            .entries
            .first()
            .is_some_and(|entry| entry.attempt != attempt)
        {
            return Err(CaptureError::Binding);
        }
        let charge = TRACE_ENTRY_FIXED_BYTES
            .checked_add(attempt.len())
            .and_then(|n| n.checked_add(id.len()))
            .and_then(|n| n.checked_add(body_bytes))
            .ok_or(CaptureError::Limit)?;
        if self.entries.len() >= MAX_TRACE_ENTRIES
            || charge > MAX_TRACE_BYTES.saturating_sub(self.used)
        {
            return Err(CaptureError::Limit);
        }
        Ok(charge)
    }
    fn require_opening(&self) -> Result<(), CaptureError> {
        if self.entries.is_empty() && !self.restored {
            return Err(CaptureError::Pending);
        }
        Ok(())
    }
    pub fn reserve_prompt(
        &mut self,
        session: &str,
        attempt: &str,
        text: &str,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        if self.restored || !self.entries.is_empty() {
            return Err(CaptureError::Invalid);
        }
        self.reserve_text(TraceKind::OpeningPrompt, attempt, session, text)
    }
    pub fn reserve_instruction(
        &mut self,
        attempt: &str,
        id: &str,
        text: &str,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        self.require_opening()?;
        if self
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, TraceKind::Instruction) && entry.id == id)
        {
            return Err(CaptureError::Invalid);
        }
        self.reserve_text(TraceKind::Instruction, attempt, id, text)
    }
    fn reserve_text(
        &mut self,
        kind: TraceKind,
        attempt: &str,
        id: &str,
        text: &str,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES {
            return Err(CaptureError::Limit);
        }
        let charge = self.capacity(attempt, id, text.len())?;
        let acknowledgment = if matches!(kind, TraceKind::Instruction) {
            Acknowledgment::Submitted
        } else {
            Acknowledgment::NotRequired
        };
        let entry = TraceEntry {
            kind,
            attempt: attempt.into(),
            id: id.into(),
            body: text.as_bytes().to_vec(),
            digest: ContextDigest::of(text.as_bytes()),
            acknowledgment,
        };
        Ok(TraceReservation {
            trace: self,
            entry,
            charge,
        })
    }
    pub fn reserve_result(
        &mut self,
        attempt: &str,
        id: &str,
        provenance: ResultProvenance,
        result: &Value,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        let mut nodes = MAX_TRACE_BYTES;
        inspect_json(result, 0, &mut nodes)?;
        self.reserve_encoded_result(attempt, id, provenance, result)
    }
    /// Typed borrowed outcome is measured before any result copy.
    pub fn reserve_helper_result(
        &mut self,
        attempt: &str,
        id: &str,
        digest: [u8; 32],
        result: &foundry_tools::host::Outcome,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        if let foundry_tools::host::Outcome::Ok { output } = result {
            let mut nodes = MAX_TRACE_BYTES;
            inspect_json(output, 0, &mut nodes)?;
        }
        self.reserve_encoded_result(
            attempt,
            id,
            ResultProvenance::PersistedHelper {
                ledger_output_digest: digest,
            },
            result,
        )
    }
    fn reserve_encoded_result(
        &mut self,
        attempt: &str,
        id: &str,
        provenance: ResultProvenance,
        result: &impl serde::Serialize,
    ) -> Result<TraceReservation<'_>, CaptureError> {
        self.require_opening()?;
        if self.entries.iter().any(|entry| {
            matches!(entry.kind, TraceKind::ToolResult(_))
                && entry.attempt == attempt
                && entry.id == id
        }) {
            return Err(CaptureError::Invalid);
        }
        let fixed = self.capacity(attempt, id, 0)?;
        let remaining = MAX_TRACE_BYTES - self.used - fixed;
        let length = bounds::measure(result, remaining)?;
        let charge = self.capacity(attempt, id, length)?;
        let body = bounds::json(result, length)?;
        let digest = ContextDigest::of(&body);
        let entry = TraceEntry {
            kind: TraceKind::ToolResult(provenance),
            attempt: attempt.into(),
            id: id.into(),
            body,
            digest,
            acknowledgment: Acknowledgment::NotRequired,
        };
        Ok(TraceReservation {
            trace: self,
            entry,
            charge,
        })
    }
    pub fn observe_instruction_receipt(
        &mut self,
        attempt: &str,
        id: &str,
        persistence_job: CheckpointId,
    ) -> Result<(), CaptureError> {
        let entry = self.instruction(attempt, id)?;
        match entry.acknowledgment {
            Acknowledgment::Submitted => {
                entry.acknowledgment = Acknowledgment::Receipt(persistence_job)
            }
            Acknowledgment::Receipt(job) if job == persistence_job => {}
            Acknowledgment::Durable => {}
            _ => return Err(CaptureError::Pending),
        }
        Ok(())
    }
    pub fn confirm_instruction_persisted(
        &mut self,
        attempt: &str,
        id: &str,
        persistence_job: CheckpointId,
    ) -> Result<(), CaptureError> {
        let entry = self.instruction(attempt, id)?;
        match entry.acknowledgment {
            Acknowledgment::Receipt(job) if job == persistence_job => {
                entry.acknowledgment = Acknowledgment::Durable
            }
            Acknowledgment::Durable => {}
            _ => return Err(CaptureError::Pending),
        }
        Ok(())
    }
    fn instruction(&mut self, attempt: &str, id: &str) -> Result<&mut TraceEntry, CaptureError> {
        self.entries
            .iter_mut()
            .find(|entry| {
                matches!(entry.kind, TraceKind::Instruction)
                    && entry.attempt == attempt
                    && entry.id == id
            })
            .ok_or(CaptureError::Binding)
    }
    pub(super) fn digest(&self) -> Result<ContextDigest, CaptureError> {
        let mut encoded = Encoding::new(b"foundry.capture.trace.v1", MAX_TRACE_BYTES)?;
        for entry in &self.entries {
            let (kind, ledger) = match &entry.kind {
                TraceKind::OpeningPrompt => (0, None),
                TraceKind::Instruction => (1, None),
                TraceKind::ToolResult(ResultProvenance::PersistedHelper {
                    ledger_output_digest,
                }) => (2, Some(ledger_output_digest)),
                TraceKind::ToolResult(ResultProvenance::SyntheticNoEffect) => (3, None),
                TraceKind::ToolResult(ResultProvenance::Denied) => (4, None),
            };
            encoded.field(&[kind, u8::from(entry.instruction_is_durable())])?;
            encoded.field(entry.attempt.as_bytes())?;
            encoded.field(entry.id.as_bytes())?;
            encoded.field(&(entry.body.len() as u64).to_be_bytes())?;
            encoded.field(entry.digest.to_hex().as_bytes())?;
            if let Some(ledger) = ledger {
                encoded.field(ledger)?;
            }
        }
        Ok(encoded.digest())
    }
}

/// Exclusive borrow prevents a second reservation or seal until commit/drop.
pub struct TraceReservation<'a> {
    trace: &'a mut DeliveredTrace,
    entry: TraceEntry,
    charge: usize,
}
impl TraceReservation<'_> {
    pub fn bytes(&self) -> &[u8] {
        &self.entry.body
    }
    /// Call only after successful pipe enqueue; this is not native acceptance proof.
    pub fn commit_enqueued(self) {
        self.trace.used += self.charge;
        self.trace.entries.push(self.entry);
    }
}

fn inspect_json(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), CaptureError> {
    if depth > 64 || *nodes == 0 {
        return Err(CaptureError::Limit);
    }
    *nodes -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                inspect_json(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                inspect_json(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
