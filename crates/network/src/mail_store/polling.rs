use super::*;
/// One item in an atomic bounded inbox result. A nonpeek read has no injection acknowledgement token.
pub struct PollRead {
    pub message: Message,
    pub attempt: Option<Attempt>,
}
impl MailStore {
    /// The single-instance host calls this once at startup. ACK tokens belong
    /// to dead IPC connections; retiring them does not consume or delete mail.
    pub(crate) fn recover_polling(&mut self) -> Result<()> {
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM poll_attempt", [])?;
        tx.execute("UPDATE mail SET attempt=NULL,session=NULL,expires=NULL WHERE state=0 AND read_ms IS NULL AND session LIKE 'ipc:%'", [])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    pub(super) fn finish_peek(
        &mut self,
        token: &Attempt,
        session: &str,
        now: i64,
        delivered: bool,
    ) -> Result<()> {
        identifier(session)?;
        timestamp(now)?;
        if session != token.session {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM poll_attempt p JOIN mail m ON m.id=p.message_id WHERE p.id=?1 AND p.message_id=?2 AND p.session=?3 AND p.expires>?4 AND m.state=0 AND m.read_ms IS NULL)", params![token.id,token.message_id,session,now], |r| r.get(0))?;
        if !valid {
            return Err(MailError::StaleAttempt);
        }
        if delivered {
            tx.execute(
                "UPDATE mail SET state=3,attempt=NULL,session=NULL,expires=NULL WHERE id=?1",
                [&token.message_id],
            )?;
            tx.execute(
                "DELETE FROM poll_attempt WHERE message_id=?1",
                [&token.message_id],
            )?;
        } else {
            tx.execute("DELETE FROM poll_attempt WHERE id=?1", [token.id])?;
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
}
