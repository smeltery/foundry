use crate::{
    ContextCheckpointId, ContinueReceipt, Result, checkpoint_codec::identifier,
    checkpoint_cut::digest, conflict,
};

/// Prebound assertions, not native execution authority. Contains no raw receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct RestoreContinueIntentV1 {
    pub session_id: String,
    pub restore_id: String,
    pub load_id: String,
    pub continuation_id: String,
    pub baseline_checkpoint_id: ContextCheckpointId,
    pub baseline_record_digest: [u8; 32],
    pub baseline_witness_digest: [u8; 32],
    pub baseline_validation_binding: [u8; 32],
    pub configuration_digest: [u8; 32],
    pub message_id: String,
    pub message_sequence: u64,
    pub message_text_digest: [u8; 32],
    pub message_text_bytes: u64,
    pub mode: RestoreContinueMode,
    pub codec: RestoreContinueCodec,
    pub tail_receipt_digest: [u8; 32],
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RestoreContinueMode {
    QueuedInstruction,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RestoreContinueCodec {
    RestoreContinueV2,
}
#[derive(Clone)]
pub struct ContinueAcceptanceInput {
    pub authorization: ContinueReceipt,
    pub intent: RestoreContinueIntentV1,
    pub authorization_binding: [u8; 32],
    pub observed_tail_receipt_digest: [u8; 32],
}
/// Historical metadata only. Never a permit or an attempt credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueAcceptanceRef {
    pub session_id: String,
    pub restore_id: String,
    pub attempt_id: String,
    pub message_id: String,
    pub request_digest: [u8; 32],
    pub record_digest: [u8; 32],
    pub event_id: String,
    pub event_sequence: u64,
    pub committed_ms: i64,
}
pub(crate) fn uuid(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(())
    } else {
        Err(conflict("invalid canonical acceptance identity"))
    }
}
impl RestoreContinueIntentV1 {
    pub(crate) fn validate(&self) -> Result<()> {
        for id in [
            &self.session_id,
            &self.restore_id,
            &self.load_id,
            &self.baseline_checkpoint_id.to_string(),
        ] {
            uuid(id)?;
        }
        for id in [&self.continuation_id, &self.message_id] {
            identifier(id)?;
        }
        if self.message_sequence == 0
            || self.message_sequence > i64::MAX as u64
            || !(1..=262144).contains(&self.message_text_bytes)
        {
            return Err(conflict("invalid acceptance message bounds"));
        }
        Ok(())
    }
    /// Compute before authorization; delivery_digest binds these exact fields.
    pub fn delivery_digest(&self) -> Result<[u8; 32]> {
        self.validate()?;
        Ok(digest(
            b"foundry.restore.intent.v1",
            &[
                self.session_id.as_bytes(),
                self.restore_id.as_bytes(),
                self.load_id.as_bytes(),
                self.continuation_id.as_bytes(),
                self.baseline_checkpoint_id.as_str().as_bytes(),
                &self.baseline_record_digest,
                &self.baseline_witness_digest,
                &self.baseline_validation_binding,
                &self.configuration_digest,
                self.message_id.as_bytes(),
                &self.message_sequence.to_be_bytes(),
                &self.message_text_digest,
                &self.message_text_bytes.to_be_bytes(),
                b"queued_instruction",
                b"restore_continue_v2",
                &self.tail_receipt_digest,
            ],
        ))
    }
}
impl ContinueAcceptanceInput {
    /// Pure computation; does not establish authorization or native receipt truth.
    pub fn computed_authorization_binding(&self) -> Result<[u8; 32]> {
        Ok(digest(
            b"foundry.restore.authorization.v1",
            &[
                &self.intent.delivery_digest()?,
                &crate::continue_codec::authorization(&self.authorization)?,
            ],
        ))
    }
    pub fn request_digest(&self) -> Result<[u8; 32]> {
        self.validate()?;
        Ok(digest(
            b"foundry.restore.acceptance.request.v1",
            &[
                &crate::continue_codec::authorization(&self.authorization)?,
                &crate::continue_codec::encode(self)?,
            ],
        ))
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let i = &self.intent;
        let a = &self.authorization;
        i.validate()?;
        uuid(&a.attempt_id)?;
        let m = a
            .message
            .as_ref()
            .ok_or_else(|| conflict("acceptance requires a bound instruction"))?;
        if a.restore_id != i.restore_id
            || a.request_id != i.continuation_id
            || a.checkpoint_id != i.baseline_checkpoint_id
            || a.delivery_digest != i.delivery_digest()?
            || m.message_id != i.message_id
            || m.sequence != i.message_sequence
            || m.text_digest != i.message_text_digest
            || m.text_bytes != i.message_text_bytes
            || a.session_version == 0
            || a.admitted_work_revision == 0
            || a.session_version > i64::MAX as u64
            || a.admitted_work_revision > i64::MAX as u64
            || self.observed_tail_receipt_digest != i.tail_receipt_digest
            || self.authorization_binding != self.computed_authorization_binding()?
        {
            return Err(conflict("acceptance authorization or prebinding mismatch"));
        }
        Ok(())
    }
}
