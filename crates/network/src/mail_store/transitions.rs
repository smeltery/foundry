use super::*;
impl MailStore {
    /// Claims one eligible row atomically. Clock is caller-supplied epoch milliseconds.
    /// Lease bounds are 1ms..=5min. Each claim consumes one of eight attempts.
    /// Lost attempts wait until lease expiry plus backoff; clock rollback delays
    /// recovery. Exhausted rows remain unread and require explicit `resume`.
    pub fn claim(
        &mut self,
        recipient: &str,
        session: &str,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<Delivery>> {
        identifier(recipient)?;
        identifier(session)?;
        timestamp(now_ms)?;
        if !(1..=300_000).contains(&lease_ms) {
            return Err(MailError::InvalidInput);
        }
        let expires = now_ms
            .checked_add(lease_ms)
            .ok_or(MailError::InvalidInput)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mail = tx.query_row("SELECT id,sender,recipient,body,reply_to,wake,notify,encoding,state,tries,next_ms FROM mail WHERE recipient=?1 AND state=0 AND read_ms IS NULL AND tries<8 AND next_ms<=?2 AND (attempt IS NULL OR expires<=?2) ORDER BY rowid LIMIT 1", params![recipient,now_ms], read_stored).optional()?;
        let Some(stored) = mail else {
            return Ok(None);
        };
        let mail = stored.message;
        // Reserve recovery delay at claim time, covering crash and disconnect too.
        let next_ms = expires
            .checked_add(retry::delay(stored.retry.attempts + 1))
            .ok_or(MailError::InvalidInput)?;
        let previous: i64 =
            tx.query_row("SELECT value FROM sequence WHERE id=1", [], |r| r.get(0))?;
        let id = previous.checked_add(1).ok_or(MailError::Capacity)?;
        tx.execute("UPDATE sequence SET value=?1 WHERE id=1", [id])?;
        tx.execute(
            "UPDATE mail SET attempt=?1,session=?2,expires=?3,tries=tries+1,next_ms=?5 WHERE id=?4",
            params![id, session, expires, mail.id, next_ms],
        )?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(Some(Delivery {
            attempt: Attempt {
                id,
                message_id: mail.id.clone(),
                session: session.into(),
                expires_ms: expires,
                kind: AttemptKind::Push,
            },
            mail,
        }))
    }
    /// Session must come from the authenticated connection, not request payload.
    pub fn acknowledge(&mut self, attempt: &Attempt, session: &str, now_ms: i64) -> Result<()> {
        match attempt.kind {
            AttemptKind::Peek => self.finish_peek(attempt, session, now_ms, true),
            AttemptKind::Push => self.finish(attempt, session, now_ms, true),
        }
    }
    /// Schedules the next claim after exponential backoff, capped at 60 seconds.
    /// Only the active, unexpired session-bound attempt may report failure.
    pub fn fail(&mut self, attempt: &Attempt, session: &str, now_ms: i64) -> Result<()> {
        match attempt.kind {
            AttemptKind::Peek => self.finish_peek(attempt, session, now_ms, false),
            AttemptKind::Push => self.finish(attempt, session, now_ms, false),
        }
    }
    fn finish(
        &mut self,
        attempt: &Attempt,
        session: &str,
        now_ms: i64,
        delivered: bool,
    ) -> Result<()> {
        identifier(session)?;
        timestamp(now_ms)?;
        if session != attempt.session {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tries: Option<u32> = tx.query_row("SELECT tries FROM mail WHERE id=?1 AND state=0 AND read_ms IS NULL AND attempt=?2 AND session=?3 AND expires>?4", params![attempt.message_id,attempt.id,session,now_ms], |r| r.get(0)).optional()?;
        let tries = tries.ok_or(MailError::StaleAttempt)?;
        let next_ms = if delivered {
            0
        } else {
            now_ms
                .checked_add(retry::delay(tries))
                .ok_or(MailError::InvalidInput)?
        };
        let changed = tx.execute("UPDATE mail SET state=?1,attempt=NULL,session=NULL,expires=NULL,next_ms=?6 WHERE id=?2 AND state=0 AND read_ms IS NULL AND attempt=?3 AND session=?4 AND expires>?5", params![if delivered {3} else {0},attempt.message_id,attempt.id,session,now_ms,next_ms])?;
        if changed != 1 {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    /// Release only this session's attempts; preserve their reserved retry delays.
    pub fn disconnect(&mut self, session: &str) -> Result<usize> {
        identifier(session)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = tx.execute(
            "UPDATE mail SET attempt=NULL,session=NULL,expires=NULL WHERE state=0 AND session=?1",
            [session],
        )?;
        let peek_count = tx.execute("DELETE FROM poll_attempt WHERE session=?1", [session])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(count + peek_count)
    }
    /// Held messages can be released or rejected once. Other states cannot settle.
    pub fn settle(&mut self, id: &str, decision: Settlement) -> Result<()> {
        identifier(id)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE mail SET state=?1 WHERE id=?2 AND state=1",
            params![
                match decision {
                    Settlement::Release => 0,
                    Settlement::Reject => 2,
                },
                id
            ],
        )? != 1
        {
            return Err(MailError::InvalidTransition);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
}
