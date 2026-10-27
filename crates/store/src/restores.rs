use crate::{
    BeginRestoreInput, BeginRestoreResult, RestoreClose, RestoreFence, RestoreRef, Result, Store,
    admission, checkpoint_codec, checkpoint_read, conflict, now_ms, read_session, restore_read,
};
use foundry_contracts::CancelMode;
use rusqlite::{Connection, TransactionBehavior, params};
use uuid::Uuid;

/// Logical retained history limits; no automatic deletion or disk/RSS guarantee.
pub const MAX_RESTORE_ROWS_PER_SESSION: i64 = 1_024;
pub const MAX_RESTORE_ROWS_PER_DATABASE: i64 = 65_536;
pub const RESTORE_RECOVERY_BATCH_SIZE: i64 = 128;

impl Store {
    /// Acquire loading ownership only. No task admission, session changes or native validation.
    pub fn begin_restore(
        &mut self,
        session_id: &str,
        input: BeginRestoreInput,
    ) -> Result<BeginRestoreResult> {
        checkpoint_codec::identifier(&input.request_id)?;
        let version = i64::try_from(input.expected_session_version)
            .map_err(|_| conflict("restore version exceeds durable integer range"))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(row) = restore_read::by_request(&tx, session_id, &input.request_id)? {
            if row.begin_digest
                != restore_read::begin_digest(session_id, &input, row.checkpoint_digest)
            {
                return Err(conflict("restore request has different content"));
            }
            let result = BeginRestoreResult::Existing(Box::new(row.reference()?));
            tx.commit()?;
            return Ok(result);
        }
        check_capacity(&tx, session_id)?;
        admission::exclusive(&tx, session_id, None)?;
        let session = read_session(&tx, session_id)?;
        if session.version != input.expected_session_version || session.active_attempt_id.is_some()
        {
            return Err(conflict("restore session version or owner changed"));
        }
        let head = checkpoint_read::head(&tx, session_id)?
            .ok_or_else(|| conflict("restore requires a current checkpoint"))?
            .private()?
            .reference;
        if head.id != input.checkpoint_id {
            return Err(conflict("restore checkpoint is not current head"));
        }
        let fence = RestoreFence {
            id: Uuid::new_v4().to_string(),
            session: session_id.into(),
            token: Uuid::new_v4().to_string(),
        };
        let digest = restore_read::begin_digest(session_id, &input, head.request_digest);
        tx.execute(
            "INSERT INTO context_restores(id,session_id,checkpoint_id,checkpoint_request_digest,
             configuration_digest,request_id,begin_digest,expected_session_version,token,expires_ms,phase)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'loading')",
            params![fence.id,session_id,head.id.as_str(),head.request_digest.as_slice(),
                input.configuration_digest.as_slice(),input.request_id,digest.as_slice(),version,
                fence.token,admission::deadline(self.lease_duration)?],
        )?;
        let row = restore_read::load(&tx, &fence.id)?;
        let checkpoint = restore_read::selection(&tx, &row, input.configuration_digest)?;
        row.owner(&fence, false)?;
        tx.commit()?;
        Ok(BeginRestoreResult::Started {
            fence,
            checkpoint: Box::new(checkpoint),
        })
    }

    pub fn heartbeat_restore(&mut self, fence: &RestoreFence) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        restore_read::load(&tx, &fence.id)?.owner(fence, true)?;
        changed(tx.execute(
            "UPDATE context_restores SET expires_ms=?1 WHERE id=?2 AND token=?3
             AND phase IN ('loading','restored') AND expires_ms>?4",
            params![
                admission::deadline(self.lease_duration)?,
                fence.id,
                fence.token,
                now_ms()?
            ],
        )?)?;
        tx.commit()?;
        Ok(())
    }
    pub fn restore_cancellation(&self, fence: &RestoreFence) -> Result<Option<CancelMode>> {
        let tx = self.connection.unchecked_transaction()?;
        let row = restore_read::load(&tx, &fence.id)?;
        row.owner(fence, true)?;
        let cancel = row.cancellation()?;
        tx.commit()?;
        Ok(cancel)
    }

    /// Trusted owner reports successful native loading. This admits no execution.
    pub fn confirm_restored(
        &mut self,
        fence: &RestoreFence,
        configuration_digest: [u8; 32],
    ) -> Result<RestoreRef> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = restore_read::load(&tx, &fence.id)?;
        row.owner(fence, false)?;
        restore_read::selection(&tx, &row, configuration_digest)?;
        changed(tx.execute(
            "UPDATE context_restores SET phase='restored' WHERE id=?1 AND token=?2
             AND phase IN ('loading','restored') AND cancel IS NULL AND expires_ms>?3",
            params![fence.id, fence.token, now_ms()?],
        )?)?;
        let result = restore_read::load(&tx, &fence.id)?.reference()?;
        tx.commit()?;
        Ok(result)
    }

    /// Release a loading owner after cleanup, preserving the exact prior session body.
    pub fn close_restore(
        &mut self,
        fence: &RestoreFence,
        close: RestoreClose,
    ) -> Result<RestoreRef> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = restore_read::load(&tx, &fence.id)?;
        row.owner(fence, true)?;
        let phase = if row.cancel.is_some() {
            "cancelled"
        } else {
            match close {
                RestoreClose::Closed => "closed",
                RestoreClose::Failed => "failed",
            }
        };
        changed(tx.execute(
            "UPDATE context_restores SET phase=?1 WHERE id=?2 AND token=?3
             AND phase IN ('loading','restored') AND expires_ms>?4",
            params![phase, fence.id, fence.token, now_ms()?],
        )?)?;
        let result = restore_read::load(&tx, &fence.id)?.reference()?;
        tx.commit()?;
        Ok(result)
    }
    /// Metadata only; owner credentials and Continue permits are never reissued.
    pub fn restore_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<RestoreRef>> {
        checkpoint_codec::identifier(request_id)?;
        let tx = self.connection.unchecked_transaction()?;
        read_session(&tx, session_id)?;
        let result = restore_read::by_request(&tx, session_id, request_id)?
            .map(|row| row.reference())
            .transpose()?;
        tx.commit()?;
        Ok(result)
    }
    /// Reclaim at most RESTORE_RECOVERY_BATCH_SIZE loading/restored leases, ordered by
    /// expiry then ID. Repeat calls to drain remaining work. Returns only after commit.
    /// Continuing uses ordinary attempt recovery; this is not a database/RSS limit.
    pub fn recover_expired_restores(&mut self) -> Result<Vec<RestoreRef>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms()?;
        let ids = {
            let mut query = tx.prepare(
                "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))=36 THEN id END
                 FROM context_restores WHERE phase IN ('loading','restored') AND expires_ms<=?1
                 ORDER BY expires_ms,id COLLATE BINARY LIMIT ?2",
            )?;
            query
                .query_map([now, RESTORE_RECOVERY_BATCH_SIZE], |row| {
                    row.get::<_, Option<String>>(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut result = Vec::new();
        for id in ids {
            let id = id.ok_or_else(|| conflict("invalid restore identity"))?;
            restore_read::load(&tx, &id)?;
            changed(tx.execute(
                "UPDATE context_restores SET phase='expired' WHERE id=?1",
                [&id],
            )?)?;
            result.push(restore_read::load(&tx, &id)?.reference()?);
        }
        tx.commit()?;
        Ok(result)
    }
}
pub(crate) fn changed(count: usize) -> Result<()> {
    if count != 1 {
        return Err(conflict("restore owner expired or phase changed"));
    }
    Ok(())
}
pub(crate) fn cancel_reserved(
    connection: &Connection,
    session: &str,
    mode: CancelMode,
) -> Result<bool> {
    let count = connection.execute(
        "UPDATE context_restores SET cancel=CASE WHEN cancel='hard' THEN cancel ELSE ?1 END
         WHERE session_id=?2 AND phase IN ('loading','restored')",
        params![
            if mode == CancelMode::Hard {
                "hard"
            } else {
                "graceful"
            },
            session
        ],
    )?;
    if count > 1 {
        return Err(conflict("multiple restore owners"));
    }
    Ok(count == 1)
}

fn check_capacity(connection: &Connection, session: &str) -> Result<()> {
    let (session_rows, database_rows): (i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM context_restores WHERE session_id=?1),
                (SELECT COUNT(*) FROM context_restores)",
        [session],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if session_rows >= MAX_RESTORE_ROWS_PER_SESSION
        || database_rows >= MAX_RESTORE_ROWS_PER_DATABASE
    {
        return Err(conflict("retained restore row quota exceeded"));
    }
    Ok(())
}
