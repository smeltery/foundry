use super::*;

pub(super) fn delay(attempts: u32) -> i64 {
    (RETRY_BASE_MS * (1_i64 << attempts.saturating_sub(1).min(6))).min(RETRY_CAP_MS)
}
impl MailStore {
    /// Explicit caller-requested retry after exhaustion. Does not change message
    /// identity or terminal state. Caller authorizes this action; no automatic reset.
    /// Active leases cannot be reset. Expired tokens are invalidated atomically.
    pub fn resume(&mut self, id: &str, now_ms: i64) -> Result<()> {
        identifier(id)?;
        timestamp(now_ms)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("UPDATE mail SET tries=0,next_ms=?1,attempt=NULL,session=NULL,expires=NULL WHERE id=?2 AND state=0 AND read_ms IS NULL AND tries=8 AND (attempt IS NULL OR expires<=?1)", params![now_ms,id])? != 1 {
            return Err(MailError::InvalidTransition);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
}
