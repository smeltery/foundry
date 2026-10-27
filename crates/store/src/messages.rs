use crate::{AttemptFence, Result, Store, attempts::check_fence, conflict, read_session};
use foundry_contracts::{ContractError, MAX_PROMPT_BYTES, Message, MessageState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

impl Store {
    /// Persist an instruction without starting execution. Retries retain its original state.
    pub fn enqueue_message(&mut self, session_id: &str, id: &str, text: &str) -> Result<Message> {
        if id.trim().is_empty() || text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES {
            return Err(ContractError::Invalid(
                "message requires an ID and 1 to 262144 UTF-8 bytes".into(),
            )
            .into());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_session(&tx, session_id)?;
        if let Some(existing) = find(&tx, session_id, id)? {
            if existing.text != text {
                return Err(conflict("message ID already has different content"));
            }
            return Ok(existing);
        }
        let last: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )?;
        let sequence = last
            .checked_add(1)
            .ok_or_else(|| conflict("message sequence exhausted"))?;
        tx.execute(
            "INSERT INTO messages(session_id,id,sequence,text,state) VALUES(?1,?2,?3,?4,'pending')",
            params![session_id, id, sequence, text],
        )?;
        let message = find(&tx, session_id, id)?.expect("inserted message exists in transaction");
        tx.commit()?;
        Ok(message)
    }

    pub fn messages(&self, session_id: &str, after: u64, limit: u32) -> Result<Vec<Message>> {
        read_session(&self.connection, session_id)?;
        let after = i64::try_from(after).map_err(|_| {
            ContractError::Invalid("message cursor exceeds durable integer range".into())
        })?;
        let mut statement = self.connection.prepare("SELECT id,session_id,sequence,text,state,attempt_id FROM messages WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
        let rows = statement.query_map(params![session_id, after, limit], decode)?;
        rows.map(|row| Ok(row?)).collect()
    }

    /// Claim before sending, preventing withdrawal during delivery. An uncertain
    /// in-flight instruction blocks later claims until explicitly reconciled.
    pub fn claim_next_message(&mut self, fence: &AttemptFence) -> Result<Option<Message>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let message = claim_next(&tx, fence)?;
        tx.commit()?;
        Ok(message)
    }

    /// Mark delivery only after the adapter acknowledges it. This is not execution success.
    /// A crash before acknowledgment may require reconciliation before redelivery.
    pub fn acknowledge_message(&mut self, fence: &AttemptFence, id: &str) -> Result<Message> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_fence(&tx, fence)?;
        let mut message = required(&tx, &fence.session_id, id)?;
        match message.state {
            MessageState::Withdrawn | MessageState::Pending => {
                return Err(conflict("message must be claimed before delivery"));
            }
            _ if message.attempt_id.as_deref() != Some(&fence.attempt_id) => {
                return Err(conflict("message belongs to another attempt"));
            }
            MessageState::Delivered => return Ok(message),
            MessageState::InFlight => {}
        }
        crate::work::advance(&tx, &fence.session_id)?;
        tx.execute(
            "UPDATE messages SET state='delivered',attempt_id=?3 WHERE session_id=?1 AND id=?2",
            params![fence.session_id, id, fence.attempt_id],
        )?;
        message.state = MessageState::Delivered;
        message.attempt_id = Some(fence.attempt_id.clone());
        tx.commit()?;
        Ok(message)
    }

    /// Withdraw only a message that has not been claimed for delivery.
    pub fn withdraw_message(&mut self, session_id: &str, id: &str) -> Result<Message> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut message = required(&tx, session_id, id)?;
        if matches!(
            message.state,
            MessageState::Delivered | MessageState::InFlight
        ) {
            return Err(conflict("claimed message cannot be withdrawn"));
        }
        tx.execute(
            "UPDATE messages SET state='withdrawn' WHERE session_id=?1 AND id=?2",
            params![session_id, id],
        )?;
        message.state = MessageState::Withdrawn;
        tx.commit()?;
        Ok(message)
    }
}

fn find(connection: &Connection, session_id: &str, id: &str) -> Result<Option<Message>> {
    Ok(connection.query_row("SELECT id,session_id,sequence,text,state,attempt_id FROM messages WHERE session_id=?1 AND id=?2", params![session_id,id], decode).optional()?)
}

fn required(connection: &Connection, session_id: &str, id: &str) -> Result<Message> {
    find(connection, session_id, id)?
        .ok_or_else(|| ContractError::NotFound("message".into()).into())
}

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let state: String = row.get(4)?;
    let state = match state.as_str() {
        "pending" => MessageState::Pending,
        "in_flight" => MessageState::InFlight,
        "delivered" => MessageState::Delivered,
        "withdrawn" => MessageState::Withdrawn,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        sequence: row.get(2)?,
        text: row.get(3)?,
        state,
        attempt_id: row.get(5)?,
    })
}

pub(crate) fn claim_next(
    connection: &rusqlite::Transaction<'_>,
    fence: &AttemptFence,
) -> Result<Option<Message>> {
    check_fence(connection, fence)?;
    let cancelled: bool = connection.query_row(
        "SELECT cancel IS NOT NULL FROM attempts WHERE id=?1",
        [&fence.attempt_id],
        |row| row.get(0),
    )?;
    if cancelled {
        return Ok(None);
    }
    let busy: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND state='in_flight')",
        [&fence.session_id],
        |row| row.get(0),
    )?;
    if busy {
        return Ok(None);
    }
    let next = connection.query_row(
        "SELECT id,session_id,sequence,text,state,attempt_id FROM messages WHERE session_id=?1 AND state='pending' ORDER BY sequence LIMIT 1",
        [&fence.session_id], decode).optional()?;
    let Some(mut message) = next else {
        return Ok(None);
    };
    crate::work::advance(connection, &fence.session_id)?;
    let changed = connection.execute(
        "UPDATE messages SET state='in_flight',attempt_id=?3 WHERE session_id=?1 AND id=?2 AND state='pending'",
        params![fence.session_id, message.id, fence.attempt_id],
    )?;
    if changed != 1 {
        return Err(conflict("message claim changed concurrently"));
    }
    message.state = MessageState::InFlight;
    message.attempt_id = Some(fence.attempt_id.clone());
    Ok(Some(message))
}
