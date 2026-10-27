use crate::{
    BeginRestoreInput, CheckpointRef, ContextCheckpointId, ContinueInput, ContinueReceipt,
    RestoreFence, RestorePhase, RestoreRef, Result, admission, checkpoint_cut, checkpoint_read,
    conflict, now_ms, read_session,
};
use foundry_contracts::CancelMode;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) struct Row {
    pub id: String,
    pub session: String,
    pub checkpoint: String,
    pub checkpoint_digest: [u8; 32],
    pub configuration: [u8; 32],
    pub request: String,
    pub begin_digest: [u8; 32],
    pub version: i64,
    pub token: String,
    pub expires: i64,
    pub cancel: Option<String>,
    pub phase: String,
    pub continue_request: Option<String>,
    pub continue_digest: Option<[u8; 32]>,
    pub delivery: Option<[u8; 32]>,
    pub attempt: Option<String>,
    pub admitted_version: Option<i64>,
    pub admitted_revision: Option<i64>,
    pub receipt_digest: Option<[u8; 32]>,
    pub message: Option<crate::ContinueMessageBinding>,
}
impl Row {
    pub fn reference(&self) -> Result<RestoreRef> {
        let phase = match self.phase.as_str() {
            "loading" => RestorePhase::Loading,
            "restored" => RestorePhase::Restored,
            "continuing" => RestorePhase::Continuing,
            "closed" => RestorePhase::Closed,
            "failed" => RestorePhase::Failed,
            "cancelled" => RestorePhase::Cancelled,
            "finished" => RestorePhase::Finished,
            "expired" => RestorePhase::Expired,
            _ => return Err(conflict("invalid restore phase")),
        };
        let checkpoint_id = ContextCheckpointId::parse(&self.checkpoint)?;
        let version = number(self.version)?;
        let input = BeginRestoreInput {
            request_id: self.request.clone(),
            checkpoint_id: checkpoint_id.clone(),
            expected_session_version: version,
            configuration_digest: self.configuration,
        };
        if begin_digest(&self.session, &input, self.checkpoint_digest) != self.begin_digest {
            return Err(conflict("restore identity integrity mismatch"));
        }
        let continuation = self.receipt()?;
        if (matches!(
            phase,
            RestorePhase::Loading
                | RestorePhase::Restored
                | RestorePhase::Closed
                | RestorePhase::Failed
                | RestorePhase::Cancelled
        ) && continuation.is_some())
            || (matches!(phase, RestorePhase::Continuing | RestorePhase::Finished)
                && continuation.is_none())
        {
            return Err(conflict("restore phase and receipt disagree"));
        }
        Ok(RestoreRef {
            id: self.id.clone(),
            session_id: self.session.clone(),
            checkpoint_id,
            configuration_digest: self.configuration,
            expected_session_version: version,
            phase,
            continuation,
        })
    }
    pub fn receipt(&self) -> Result<Option<ContinueReceipt>> {
        let Some(attempt) = &self.attempt else {
            if self.message.is_some()
                || self.continue_request.is_some()
                || self.continue_digest.is_some()
                || self.delivery.is_some()
                || self.admitted_version.is_some()
                || self.admitted_revision.is_some()
                || self.receipt_digest.is_some()
            {
                return Err(conflict("incomplete restore receipt"));
            }
            return Ok(None);
        };
        let missing = || conflict("incomplete restore receipt");
        let receipt = ContinueReceipt {
            restore_id: self.id.clone(),
            attempt_id: attempt.clone(),
            request_id: self.continue_request.clone().ok_or_else(missing)?,
            delivery_digest: self.delivery.ok_or_else(missing)?,
            checkpoint_id: ContextCheckpointId::parse(&self.checkpoint)?,
            session_version: number(self.admitted_version.ok_or_else(missing)?)?,
            admitted_work_revision: number(self.admitted_revision.ok_or_else(missing)?)?,
            message: self.message.clone(),
        };
        let input = ContinueInput {
            request_id: receipt.request_id.clone(),
            configuration_digest: self.configuration,
            delivery_digest: receipt.delivery_digest,
        };
        let digest = crate::restore_message::request_digest(self, &input, self.message.as_ref());
        if self.continue_digest != Some(digest)
            || self.receipt_digest != Some(receipt_digest(&receipt, digest))
        {
            return Err(conflict("restore receipt integrity mismatch"));
        }
        Ok(Some(receipt))
    }
    pub fn credential(&self, fence: &RestoreFence) -> Result<()> {
        if self.id != fence.id || self.session != fence.session || self.token != fence.token {
            return Err(conflict("invalid restore owner"));
        }
        Ok(())
    }
    pub fn owner(&self, fence: &RestoreFence, allow_cancel: bool) -> Result<()> {
        self.credential(fence)?;
        if !matches!(self.phase.as_str(), "loading" | "restored")
            || self.expires <= now_ms()?
            || (!allow_cancel && self.cancel.is_some())
        {
            return Err(conflict(
                "restore lease expired, cancelled or no longer loading",
            ));
        }
        Ok(())
    }
    pub fn cancellation(&self) -> Result<Option<CancelMode>> {
        match self.cancel.as_deref() {
            None => Ok(None),
            Some("graceful") => Ok(Some(CancelMode::Graceful)),
            Some("hard") => Ok(Some(CancelMode::Hard)),
            _ => Err(conflict("invalid restore cancellation")),
        }
    }
}
fn number(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| conflict("invalid restore counter"))
}

pub(crate) fn begin_digest(
    session: &str,
    input: &BeginRestoreInput,
    checkpoint: [u8; 32],
) -> [u8; 32] {
    checkpoint_cut::digest(
        b"foundry.restore.begin.v1",
        &[
            session.as_bytes(),
            input.request_id.as_bytes(),
            input.checkpoint_id.as_str().as_bytes(),
            &checkpoint,
            &input.configuration_digest,
            &input.expected_session_version.to_be_bytes(),
        ],
    )
}
pub(crate) fn continue_digest(row: &Row, input: &ContinueInput) -> [u8; 32] {
    checkpoint_cut::digest(
        b"foundry.restore.continue.v1",
        &[
            row.id.as_bytes(),
            &row.begin_digest,
            input.request_id.as_bytes(),
            &input.configuration_digest,
            &input.delivery_digest,
        ],
    )
}
pub(crate) fn receipt_digest(receipt: &ContinueReceipt, request: [u8; 32]) -> [u8; 32] {
    checkpoint_cut::digest(
        if receipt.message.is_some() {
            b"foundry.restore.receipt.v2"
        } else {
            b"foundry.restore.receipt.v1"
        },
        &[
            &request,
            receipt.restore_id.as_bytes(),
            receipt.attempt_id.as_bytes(),
            &receipt.session_version.to_be_bytes(),
            &receipt.admitted_work_revision.to_be_bytes(),
        ],
    )
}
pub(crate) fn selection(
    connection: &Connection,
    row: &Row,
    configuration: [u8; 32],
) -> Result<CheckpointRef> {
    admission::exclusive(connection, &row.session, Some(&row.id))?;
    let session = read_session(connection, &row.session)?;
    if session.version != number(row.version)? || session.active_attempt_id.is_some() {
        return Err(conflict("session changed since restore began"));
    }
    let head = checkpoint_read::head(connection, &row.session)?
        .ok_or_else(|| conflict("restore checkpoint head is missing"))?
        .private()?
        .reference;
    if head.id.as_str() != row.checkpoint
        || head.request_digest != row.checkpoint_digest
        || head.configuration_digest != configuration
        || row.configuration != configuration
        || checkpoint_cut::durable_cut(connection, &row.session)? != head.cut
    {
        return Err(conflict("restore head, cut or configuration changed"));
    }
    Ok(head)
}
pub(crate) fn load(connection: &Connection, id: &str) -> Result<Row> {
    let valid: Option<bool> = connection.query_row(
        "SELECT typeof(id)='text' AND length(CAST(id AS BLOB))=36
         AND typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36
         AND typeof(checkpoint_id)='text' AND length(CAST(checkpoint_id AS BLOB))=36
         AND typeof(token)='text' AND length(CAST(token AS BLOB))=36
         AND typeof(request_id)='text' AND length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128
         AND typeof(phase)='text' AND length(CAST(phase AS BLOB)) BETWEEN 1 AND 10
         AND (cancel IS NULL OR (typeof(cancel)='text' AND length(CAST(cancel AS BLOB))<=8))
         AND (continue_request_id IS NULL OR (typeof(continue_request_id)='text' AND length(CAST(continue_request_id AS BLOB)) BETWEEN 1 AND 128))
         AND (attempt_id IS NULL OR (typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36))
         FROM context_restores WHERE id=?1", [id], |r|r.get(0),
    ).optional()?;
    if valid != Some(true) {
        return Err(conflict(
            "restore missing or stored identity exceeds bounds",
        ));
    }
    let mut row = connection.query_row(
        "SELECT id,session_id,checkpoint_id,checkpoint_request_digest,configuration_digest,request_id,
         begin_digest,expected_session_version,token,expires_ms,cancel,phase,continue_request_id,
         continue_digest,delivery_digest,attempt_id,admitted_session_version,admitted_work_revision,receipt_digest
         FROM context_restores WHERE id=?1", [id], |r|Ok(Row {
            id:r.get(0)?,session:r.get(1)?,checkpoint:r.get(2)?,checkpoint_digest:r.get(3)?,
            configuration:r.get(4)?,request:r.get(5)?,begin_digest:r.get(6)?,version:r.get(7)?,token:r.get(8)?,
            expires:r.get(9)?,cancel:r.get(10)?,phase:r.get(11)?,continue_request:r.get(12)?,
            continue_digest:r.get(13)?,delivery:r.get(14)?,attempt:r.get(15)?,admitted_version:r.get(16)?,
            admitted_revision:r.get(17)?,receipt_digest:r.get(18)?,message:None,
        }),
    )?;
    row.message = crate::restore_message::load(connection, &row)?;
    row.reference()?;
    row.cancellation()?;
    Ok(row)
}
pub(crate) fn by_request(
    connection: &Connection,
    session: &str,
    request: &str,
) -> Result<Option<Row>> {
    let id: Option<Option<String>> = connection
        .query_row(
            "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))=36 THEN id END
         FROM context_restores WHERE session_id=?1 AND request_id=?2",
            params![session, request],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        None => Ok(None),
        Some(Some(id)) => load(connection, &id).map(Some),
        Some(None) => Err(conflict("invalid restore identity")),
    }
}
