use super::*;
use std::time::Duration;
impl AttachmentStore {
    /// Source-compatible explicit age-out. Call under the host's reference gate.
    /// Expiry revokes payload/path access before removal; returned reference kind
    /// tells the host whether attachments or the entire held message must expire.
    pub fn age_out(
        &mut self,
        key: &AttachmentKey,
        reference: RetentionReference,
        now_ms: i64,
    ) -> Result<ExpiryOutcome> {
        if now_ms < 0 {
            return Err(AttachmentError::Invalid);
        }
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if batch.state == 2 {
            return Err(AttachmentError::Deleting);
        }
        let kind = if batch.state >= 3 {
            batch.expiry.ok_or(AttachmentError::Corrupt)?
        } else {
            let (start, duration, kind) = match reference {
                RetentionReference::Read { at_ms } => {
                    if at_ms < batch.accepted || at_ms > now_ms {
                        return Err(AttachmentError::Invalid);
                    }
                    (at_ms, RETAIN_RELEASED_MS, 0)
                }
                RetentionReference::Unread => (batch.accepted, RETAIN_UNREAD_MS, 1),
                RetentionReference::Held => (batch.accepted, RETAIN_UNREAD_MS, 2),
            };
            let expires_ms = start
                .checked_add(duration)
                .ok_or(AttachmentError::Invalid)?;
            if now_ms < expires_ms {
                return Ok(ExpiryOutcome::NotDue { expires_ms });
            }
            self.connection.execute(
                "UPDATE batches SET state=4,expiry=?2 WHERE id=?1",
                rusqlite::params![id, kind],
            )?;
            self.protection.sync()?;
            kind
        };
        let reference = match kind {
            0 => ExpiredReference::ReadAttachments,
            1 => ExpiredReference::UnreadAttachments,
            2 => ExpiredReference::HeldMessage,
            _ => return Err(AttachmentError::Corrupt),
        };
        if batch.state == 3 {
            return Ok(ExpiryOutcome::Expired {
                reference,
                cleanup_error: None,
            });
        }
        if let Err(error) = materialize::delete(&self.root, key, &batch.files, &batch.names) {
            return Ok(ExpiryOutcome::Expired {
                reference,
                cleanup_error: Some(error),
            });
        }
        self.protection.check()?;
        self.connection.execute(
            "UPDATE batches SET state=3,payload=?2,names=?2,bytes=0 WHERE id=?1",
            rusqlite::params![id, b"[]".as_slice()],
        )?;
        self.protection.sync()?;
        Ok(ExpiryOutcome::Expired {
            reference,
            cleanup_error: None,
        })
    }
    /// Host acknowledgement that the expired reference outcome was applied.
    /// Ends retry identity retention; callers must never reuse message identities.
    pub fn forget_expired(&mut self, key: &AttachmentKey) -> Result<()> {
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if batch.state != 3 {
            return Err(AttachmentError::InUse);
        }
        self.connection
            .execute("DELETE FROM batches WHERE id=?1 AND state=3", [&id])?;
        self.protection.sync()?;
        Ok(())
    }
}
