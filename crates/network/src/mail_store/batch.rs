use super::*;
/// All eligible messages are returned or the operation fails without consuming
/// any. This local bound is explicit; it is not an unbounded-source parity claim.
pub const MAX_INBOX_MESSAGES: usize = 128;
impl MailStore {
    pub fn poll_batch(
        &mut self,
        recipient: &str,
        session: &str,
        now: i64,
        peek: bool,
    ) -> Result<Vec<PollRead>> {
        Ok(self
            .poll_batch_checked(recipient, session, now, peek, |_| Ok(()), || true)?
            .map(|(reads, ())| reads)
            .unwrap_or_default())
    }
    /// Preparation is pure bounded validation/encoding, never adapter execution.
    /// It runs inside the same snapshot, before any consumption. A failed render,
    /// ACK-capacity check or frame budget rolls back token allocation and sequence.
    /// Cancellation is checked after obtaining SQLite and again before commit.
    pub(crate) fn poll_batch_checked<T>(
        &mut self,
        recipient: &str,
        session: &str,
        now: i64,
        peek: bool,
        prepare: impl FnOnce(&[PollRead]) -> Result<T>,
        active: impl Fn() -> bool,
    ) -> Result<Option<(Vec<PollRead>, T)>> {
        identifier(recipient)?;
        identifier(session)?;
        timestamp(now)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: usize = tx.query_row("SELECT count(*) FROM mail WHERE recipient=?1 AND state=0 AND read_ms IS NULL AND (attempt IS NULL OR expires<=?2)", params![recipient,now], |r| r.get(0))?;
        if !active() {
            return Ok(None);
        }
        if count > MAX_INBOX_MESSAGES {
            return Err(MailError::Capacity);
        }
        let messages = {
            let mut query = tx.prepare("SELECT id,sender,recipient,body,reply_to,wake,notify,encoding FROM mail WHERE recipient=?1 AND state=0 AND read_ms IS NULL AND (attempt IS NULL OR expires<=?2) ORDER BY rowid")?;
            query
                .query_map(params![recipient, now], read_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for message in &messages {
            message.validate_payload()?;
        }
        let mut reads = Vec::with_capacity(messages.len());
        for message in messages {
            let attempt = if peek {
                Some(peek_token(&tx, &message, session, now)?)
            } else {
                None
            };
            reads.push(PollRead { message, attempt });
        }
        let prepared = prepare(&reads)?;
        if !active() {
            return Ok(None);
        }
        if !peek {
            for read in &reads {
                tx.execute(
                    "UPDATE mail SET read_ms=?1,attempt=NULL,session=NULL,expires=NULL WHERE id=?2",
                    params![now, read.message.id],
                )?;
                tx.execute(
                    "DELETE FROM poll_attempt WHERE message_id=?1",
                    [&read.message.id],
                )?;
            }
        }
        self.protection.check()?;
        // This is the final authorization point. Cancellation after it cannot
        // promise to retract a commit already accepted by SQLite.
        if !active() {
            return Ok(None);
        }
        tx.commit()?;
        self.protection.sync()?;
        Ok(Some((reads, prepared)))
    }
}
fn peek_token(
    tx: &rusqlite::Transaction<'_>,
    message: &Message,
    session: &str,
    now: i64,
) -> Result<Attempt> {
    tx.execute("DELETE FROM poll_attempt WHERE expires<=?1", [now])?;
    let cached: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id,expires FROM poll_attempt WHERE message_id=?1 AND session=?2",
            params![message.id, session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (id, expires) = if let Some(cached) = cached {
        cached
    } else {
        let total: usize = tx.query_row("SELECT count(*) FROM poll_attempt", [], |r| r.get(0))?;
        let scoped: usize = tx.query_row(
            "SELECT count(*) FROM poll_attempt WHERE session=?1",
            [session],
            |r| r.get(0),
        )?;
        if total >= 16_384 || scoped >= MAX_INBOX_MESSAGES {
            return Err(MailError::Capacity);
        }
        let previous: i64 =
            tx.query_row("SELECT value FROM sequence WHERE id=1", [], |r| r.get(0))?;
        let id = previous.checked_add(1).ok_or(MailError::Capacity)?;
        tx.execute("UPDATE sequence SET value=?1 WHERE id=1", [id])?;
        tx.execute(
            "INSERT INTO poll_attempt(id,message_id,session,expires) VALUES(?1,?2,?3,?4)",
            params![id, message.id, session, i64::MAX],
        )?;
        (id, i64::MAX)
    };
    Ok(Attempt {
        id,
        message_id: message.id.clone(),
        session: session.into(),
        expires_ms: expires,
        kind: AttemptKind::Peek,
    })
}
