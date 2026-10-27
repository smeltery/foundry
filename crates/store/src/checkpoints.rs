use crate::{
    AttemptFence, CheckpointCut, CheckpointRef, ContextCheckpointId, OpaqueCheckpointInput,
    PrivateCheckpoint, Result, Store, checkpoint_codec, checkpoint_cut,
    checkpoint_read::{self, Stored},
    conflict, now_ms, read_session,
};
use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

const SESSION_BYTES: i64 = 256 * 1024 * 1024;
const DATABASE_BYTES: i64 = 1024 * 1024 * 1024;
// Payload plus logical metadata, identities, digests and integer columns; no page/WAL estimate.
const CHARGE_SQL: &str = "length(payload)+length(CAST(metadata AS BLOB))
    +length(CAST(coverage AS BLOB))+length(CAST(id AS BLOB))
    +length(CAST(session_id AS BLOB))+length(CAST(attempt_id AS BLOB))
    +length(CAST(request_id AS BLOB))+COALESCE(length(CAST(parent_id AS BLOB)),0)+152
    +COALESCE((SELECT 76 FROM context_finalizations WHERE checkpoint_id=context_checkpoints.id),0)
    +COALESCE((SELECT length(body)+184 FROM context_witnesses WHERE checkpoint_id=context_checkpoints.id),0)";

impl Store {
    /// One consistent read snapshot of durable work. This does not quiesce an adapter.
    pub fn checkpoint_cut(&self, fence: &AttemptFence) -> Result<CheckpointCut> {
        let tx = self.connection.unchecked_transaction()?;
        let cut = checkpoint_cut::cut(&tx, fence)?;
        tx.commit()?;
        Ok(cut)
    }

    /// Publish ordinary opaque data. Integrity and durable coverage checks do NOT
    /// establish native semantics, resumability, or producer annotation authority.
    pub fn commit_opaque_checkpoint(
        &mut self,
        fence: &AttemptFence,
        input: OpaqueCheckpointInput,
    ) -> Result<CheckpointRef> {
        let prepared = prepare(fence, input, None)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reference = publish(&tx, fence, prepared)?;
        tx.commit()?;
        Ok(reference)
    }

    /// Redacted head metadata; also verifies stored payload/record integrity.
    pub fn checkpoint_head(&self, session_id: &str) -> Result<Option<CheckpointRef>> {
        let tx = self.connection.unchecked_transaction()?;
        read_session(&tx, session_id)?;
        let result = checkpoint_read::head(&tx, session_id)?
            .map(|row| row.private().map(|value| value.reference))
            .transpose()?;
        tx.commit()?;
        Ok(result)
    }
    /// Private data, never automatically serialized into sessions or events.
    pub fn read_opaque_checkpoint(
        &self,
        session_id: &str,
        id: &ContextCheckpointId,
    ) -> Result<PrivateCheckpoint> {
        let tx = self.connection.unchecked_transaction()?;
        read_session(&tx, session_id)?;
        let result = checkpoint_read::load(&tx, session_id, id.as_str())?
            .ok_or_else(|| foundry_contracts::ContractError::NotFound("checkpoint".into()))?
            .private()?;
        tx.commit()?;
        Ok(result)
    }
    /// Metadata-only recovery, not acknowledgement of the caller's intended content.
    /// Compare request_digest with OpaqueCheckpointInput::request_digest for exact
    /// reconciliation. Lookup does not authorize publication or semantic restore.
    pub fn checkpoint_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<CheckpointRef>> {
        checkpoint_codec::identifier(request_id)?;
        let tx = self.connection.unchecked_transaction()?;
        read_session(&tx, session_id)?;
        let result = checkpoint_read::by_request(&tx, session_id, request_id)?
            .map(|row| row.private().map(|value| value.reference))
            .transpose()?;
        tx.commit()?;
        Ok(result)
    }
}
pub(crate) struct PreparedCheckpoint {
    pub(crate) row: Stored,
    cut: CheckpointCut,
    coverage: crate::CoverageManifest,
    expected_head: Option<ContextCheckpointId>,
    finalization: bool,
}

pub(crate) fn prepare(
    fence: &AttemptFence,
    input: OpaqueCheckpointInput,
    terminal: Option<&crate::TerminalProposal>,
) -> Result<PreparedCheckpoint> {
    let (metadata, coverage) = checkpoint_codec::encode_with_terminal(&input, terminal)?;
    let mut row = Stored {
        id: uuid::Uuid::new_v4().to_string(),
        session: fence.session_id.clone(),
        attempt: fence.attempt_id.clone(),
        generation: 1,
        parent: input
            .expected_head
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        request: input.request_id,
        metadata,
        coverage,
        configuration_digest: input.configuration_digest,
        payload_digest: Sha256::digest(&input.payload).into(),
        payload: input.payload,
        request_digest: [0; 32],
        record_digest: [0; 32],
        committed: now_ms()?,
        request_format: 1,
        witness: None,
    };
    row.request_digest = row.request_hash();
    Ok(PreparedCheckpoint {
        row,
        cut: input.cut,
        coverage: input.coverage,
        expected_head: input.expected_head,
        finalization: terminal.is_some(),
    })
}

pub(crate) fn publish(
    tx: &rusqlite::Transaction<'_>,
    fence: &AttemptFence,
    prepared: PreparedCheckpoint,
) -> Result<CheckpointRef> {
    let mut row = prepared.row;
    checkpoint_cut::owner(tx, fence)?;
    if let Some(existing) = checkpoint_read::by_request(tx, &fence.session_id, &row.request)? {
        if existing.request_digest != row.request_digest {
            return Err(conflict("checkpoint request has different content"));
        }
        return Ok(existing.private()?.reference);
    }
    let head = checkpoint_read::head(tx, &fence.session_id)?
        .map(Stored::private)
        .transpose()?;
    if head.as_ref().map(|head| &head.reference.id) != prepared.expected_head.as_ref() {
        return Err(conflict("checkpoint head changed"));
    }
    if let Some(head) = head {
        row.generation = i64::try_from(head.reference.generation)
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| conflict("checkpoint generation exhausted"))?;
    }
    if checkpoint_cut::cut(tx, fence)? != prepared.cut {
        return Err(conflict("checkpoint durable cut changed"));
    }
    checkpoint_cut::coverage(tx, &fence.session_id, &prepared.coverage)?;
    if let Some(witness) = &row.witness {
        crate::witness_storage::check_parent(tx, &row, &witness.body)?;
        crate::witness_facts::verify(tx, fence, &witness.body)?;
    }
    crate::witness_storage::check_quota_associations(tx)?;
    let quota_query = format!(
        "SELECT COALESCE(SUM(CASE WHEN session_id=?1 THEN {CHARGE_SQL} ELSE 0 END),0),
                    COALESCE(SUM({CHARGE_SQL}),0) FROM context_checkpoints"
    );
    let (session_bytes, total_bytes): (i64, i64) =
        tx.query_row(&quota_query, [&fence.session_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let bytes = (row.payload.len()
        + row.metadata.len()
        + row.coverage.len()
        + row.id.len()
        + row.session.len()
        + row.attempt.len()
        + row.request.len()
        + row.parent.as_ref().map_or(0, String::len)
        + 152
        + row
            .witness
            .as_ref()
            .map_or(0, |w| w.encoded.bytes().len() + 184)
        + if prepared.finalization { 76 } else { 0 }) as i64;
    quota(session_bytes, total_bytes, bytes)?;
    row.record_digest = row.record_hash();
    let inserted = tx.execute(
            "INSERT INTO context_checkpoints(
                id,session_id,attempt_id,generation,parent_id,request_id,metadata,coverage,
                payload,payload_digest,request_digest,record_digest,committed_ms,configuration_digest,request_format
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                row.id, row.session, row.attempt, row.generation, row.parent, row.request,
                row.metadata, row.coverage, row.payload, row.payload_digest.as_slice(),
                row.request_digest.as_slice(), row.record_digest.as_slice(), row.committed,
                row.configuration_digest.as_slice(), row.request_format,
            ],
        )?;
    if inserted != 1 {
        return Err(conflict("checkpoint insertion failed"));
    }
    crate::witness_storage::insert(tx, &row)?;
    let changed = tx.execute(
        "INSERT INTO context_checkpoint_heads(session_id,checkpoint_id) VALUES(?1,?2)
             ON CONFLICT(session_id) DO UPDATE SET checkpoint_id=excluded.checkpoint_id",
        params![row.session, row.id],
    )?;
    if changed != 1 {
        return Err(conflict("checkpoint head publication failed"));
    }
    let reference = row.private()?.reference;
    // Hashing/coverage scans can take time: expiry must still reject publication.
    checkpoint_cut::owner(tx, fence)?;

    Ok(reference)
}

fn quota(session: i64, total: i64, addition: i64) -> Result<()> {
    if session < 0
        || total < 0
        || addition < 0
        || session
            .checked_add(addition)
            .is_none_or(|n| n > SESSION_BYTES)
        || total
            .checked_add(addition)
            .is_none_or(|n| n > DATABASE_BYTES)
    {
        return Err(conflict("checkpoint quota exceeded"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_limits_are_inclusive_and_overflow_is_rejected() {
        assert!(quota(SESSION_BYTES - 1, DATABASE_BYTES - 1, 1).is_ok());
        assert!(quota(SESSION_BYTES, DATABASE_BYTES - 1, 1).is_err());
        assert!(quota(0, DATABASE_BYTES, 1).is_err());
        assert!(quota(i64::MAX, 0, 1).is_err());
        assert!(quota(0, i64::MAX, 1).is_err());
        assert!(quota(-1, 0, 1).is_err());
    }
}
