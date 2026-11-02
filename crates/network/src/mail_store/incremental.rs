use super::*;

const ELIGIBLE: &str =
    "recipient=?1 AND state=0 AND read_ms IS NULL AND (attempt IS NULL OR expires<=?2)";
const COLS: &str = "id,sender,recipient,body,reply_to,wake,notify,encoding";

pub(crate) struct InboxBatch<'a> {
    tx: &'a rusqlite::Transaction<'a>,
    recipient: &'a str,
    session: &'a str,
    now: i64,
    peek: bool,
    pub count: usize,
}
impl InboxBatch<'_> {
    /// Each pass owns only one bounded decoded row at a time. Tokens are reused
    /// on the second pass, within the same transaction and immutable snapshot.
    pub fn for_each(&self, mut visit: impl FnMut(PollRead) -> Result<()>) -> Result<()> {
        let mut query = self.tx.prepare(&format!(
            "SELECT {COLS} FROM mail WHERE {ELIGIBLE} ORDER BY rowid"
        ))?;
        let mut rows = query.query(params![self.recipient, self.now])?;
        while let Some(row) = rows.next()? {
            let message = read_message(row)?;
            validate_message(&message)?;
            let attempt = if self.peek {
                Some(batch::peek_token(self.tx, &message, self.session)?)
            } else {
                None
            };
            visit(PollRead { message, attempt })?;
        }
        Ok(())
    }
}
pub(super) fn validate_message(message: &Message) -> Result<()> {
    identifier(&message.id)?;
    identifier(&message.sender)?;
    identifier(&message.recipient)?;
    if let Some(id) = &message.reply_to {
        identifier(id)?;
    }
    if message.body.len() > message.encoding.maximum_bytes() {
        return Err(MailError::InvalidPayload);
    }
    message.validate_payload()
}
/// Validate the entire store before constructing any response. Quotas apply to
/// retained terminal rows too; malformed metadata is never silently filtered.
pub(super) fn validate_budget(tx: &rusqlite::Transaction<'_>, now: i64, peek: bool) -> Result<()> {
    let (count, bytes, invalid): (i64, i64, bool) = tx.query_row(
        "SELECT count(*),coalesce(sum(length(body)),0),coalesce(max(typeof(body)!='blob' OR encoding NOT IN(0,1) OR length(body)>CASE WHEN encoding=1 THEN ?3 ELSE ?1 END OR typeof(id)!='text' OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND ?2 OR typeof(sender)!='text' OR length(CAST(sender AS BLOB)) NOT BETWEEN 1 AND ?2 OR typeof(recipient)!='text' OR length(CAST(recipient AS BLOB)) NOT BETWEEN 1 AND ?2 OR (reply_to IS NOT NULL AND (typeof(reply_to)!='text' OR length(CAST(reply_to AS BLOB)) NOT BETWEEN 1 AND ?2))),0) FROM mail",
        params![MAX_MAIL_BODY, MAX_MAIL_IDENTIFIER,crate::codec::MAX_FRAME_BYTES], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?;
    if count > MAX_ROWS || bytes > MAX_BYTES || invalid {
        return Err(MailError::InvalidPayload);
    }
    // Check control characters in all retained identities, without reading bodies.
    let mut query = tx.prepare("SELECT id,sender,recipient,reply_to FROM mail")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        for column in 0..4 {
            let value: Option<String> = row.get(column)?;
            if let Some(value) = value {
                identifier(&value)?;
            } else if column != 3 {
                return Err(MailError::InvalidPayload);
            }
        }
    }
    if peek {
        tx.execute("DELETE FROM poll_attempt WHERE expires<=?1 OR NOT EXISTS(SELECT 1 FROM mail m WHERE m.id=poll_attempt.message_id AND m.state=0 AND m.read_ms IS NULL)", [now])?;
        let tokens: usize = tx.query_row("SELECT count(*) FROM poll_attempt", [], |r| r.get(0))?;
        if tokens > MAX_MAIL_ROWS * MAX_POLL_SCOPES {
            return Err(MailError::Capacity);
        }
    }
    Ok(())
}
/// The page envelope includes a conservative two leaf pages per token for its
/// table and unique index, plus matching interior/free-page headroom. Logical
/// mail quotas are unchanged. Journal allocation remains an explicit I/O risk.
pub(super) fn storage_budget(connection: &Connection) -> Result<()> {
    let page: u64 = connection.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    if !(512..=65536).contains(&page) || !page.is_power_of_two() {
        return Err(MailError::Schema);
    }
    let bytes = MAX_MAIL_BODY_BYTES as u64
        + MAX_MAIL_ROWS as u64 * 16_384
        + MAX_MAIL_ROWS as u64 * MAX_POLL_SCOPES as u64 * 16_384;
    let pages = bytes.div_ceil(page);
    connection.pragma_update(None, "max_page_count", pages)?;
    Ok(())
}
impl MailStore {
    pub(crate) fn poll_incremental_checked<T>(
        &mut self,
        recipient: &str,
        session: &str,
        now: i64,
        peek: bool,
        prepare: impl FnOnce(&InboxBatch<'_>) -> Result<T>,
        active: impl Fn() -> bool,
    ) -> Result<Option<T>> {
        identifier(recipient)?;
        identifier(session)?;
        timestamp(now)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !active() {
            return Ok(None);
        }
        validate_budget(&tx, now, peek)?;
        let count = tx.query_row(
            &format!("SELECT count(*) FROM mail WHERE {ELIGIBLE}"),
            params![recipient, now],
            |r| r.get(0),
        )?;
        let prepared = prepare(&InboxBatch {
            tx: &tx,
            recipient,
            session,
            now,
            peek,
            count,
        })?;
        if !active() {
            return Ok(None);
        }
        validate_budget(&tx, now, peek)?;
        if !peek {
            tx.execute(&format!("UPDATE mail SET read_ms=?3,attempt=NULL,session=NULL,expires=NULL WHERE {ELIGIBLE}"), params![recipient,now,now])?;
            tx.execute("DELETE FROM poll_attempt WHERE message_id IN(SELECT id FROM mail WHERE recipient=?1 AND read_ms IS NOT NULL)", [recipient])?;
        }
        self.protection.check()?;
        if !active() {
            return Ok(None);
        }
        tx.commit()?;
        self.protection.sync()?;
        Ok(Some(prepared))
    }
    /// Resolve only a token actually issued to this fresh connection and exact
    /// recipient. No externally constructible token or caller session is trusted.
    pub(crate) fn finish_issued_peek(
        &mut self,
        id: i64,
        session: &str,
        recipient: &str,
        now: i64,
        delivered: bool,
    ) -> Result<()> {
        self.protection.check()?;
        let (token, owner) = self.connection.query_row(
            "SELECT p.message_id,p.expires,m.recipient FROM poll_attempt p JOIN mail m ON m.id=p.message_id WHERE p.id=?1 AND p.session=?2 AND m.state=0 AND m.read_ms IS NULL",
            params![id,session], |r| Ok((Attempt {id,message_id:r.get(0)?,session:session.into(),expires_ms:r.get(1)?,kind:AttemptKind::Peek}, r.get::<_, String>(2)?)),
        ).optional()?.ok_or(MailError::StaleAttempt)?;
        if owner != recipient {
            return Err(MailError::WrongRecipient);
        }
        self.finish_peek(&token, session, now, delivered)
    }
}
