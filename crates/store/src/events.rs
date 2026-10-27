use crate::{Result, Store, attempts::check_fence, conflict, read_session};
use foundry_contracts::{ContractError, Event};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

impl Store {
    /// Sequences start at one per session. Revisions are immutable snapshots.
    /// Exact duplicate submissions return the original sequence without writing.
    pub fn append_event(
        &mut self,
        fence: &crate::AttemptFence,
        id: &str,
        revision: u64,
        kind: &str,
        data: Value,
    ) -> Result<Event> {
        if id.trim().is_empty() || kind.trim().is_empty() {
            return Err(
                ContractError::Invalid("event id and kind must not be empty".into()).into(),
            );
        }
        let revision_sql = number(revision)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_fence(&tx, fence)?;
        let previous = tx.query_row(
            "SELECT id,session_id,attempt_id,sequence,revision,kind,data FROM events WHERE session_id=?1 AND id=?2 AND revision=?3",
            params![fence.session_id, id, revision_sql], decode_row,
        ).optional()?.map(decode_data).transpose()?;
        if let Some(event) = previous {
            if event.attempt_id == fence.attempt_id && event.kind == kind && event.data == data {
                return Ok(event);
            }
            return Err(conflict("event revision already has different content"));
        }
        let latest: Option<(String,i64,String)> = tx.query_row(
            "SELECT attempt_id,revision,kind FROM events WHERE session_id=?1 AND id=?2 ORDER BY revision DESC LIMIT 1",
            params![fence.session_id,id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        if let Some((attempt, latest_revision, latest_kind)) = latest
            && (attempt != fence.attempt_id
                || latest_kind != kind
                || revision_sql <= latest_revision)
        {
            return Err(conflict(
                "event identity changed or revision did not advance",
            ));
        }
        let last: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM events WHERE session_id=?1",
            [&fence.session_id],
            |r| r.get(0),
        )?;
        let sequence = last
            .checked_add(1)
            .ok_or_else(|| conflict("event sequence exhausted"))?;
        crate::work::advance(&tx, &fence.session_id)?;
        tx.execute("INSERT INTO events(session_id,sequence,id,attempt_id,revision,kind,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![fence.session_id,sequence,id,fence.attempt_id,revision_sql,kind,serde_json::to_string(&data)?])?;
        tx.commit()?;
        Ok(Event {
            id: id.into(),
            session_id: fence.session_id.clone(),
            attempt_id: fence.attempt_id.clone(),
            sequence: sequence as u64,
            revision,
            kind: kind.into(),
            data,
        })
    }

    /// Returns all revisions in delivery order, with an exclusive sequence cursor.
    pub fn events(&self, session_id: &str, after: u64, limit: u32) -> Result<Vec<Event>> {
        read_session(&self.connection, session_id)?;
        let after = number(after)?;
        let mut statement = self.connection.prepare(
            "SELECT id,session_id,attempt_id,sequence,revision,kind,data FROM events WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
        let rows = statement.query_map(params![session_id, after, limit], decode_row)?;
        rows.map(|row| decode_data(row?)).collect()
    }
}
fn number(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| ContractError::Invalid("number exceeds durable integer range".into()).into())
}
fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Event, String)> {
    Ok((
        Event {
            id: row.get(0)?,
            session_id: row.get(1)?,
            attempt_id: row.get(2)?,
            sequence: row.get(3)?,
            revision: row.get(4)?,
            kind: row.get(5)?,
            data: Value::Null,
        },
        row.get(6)?,
    ))
}
fn decode_data((mut event, data): (Event, String)) -> Result<Event> {
    event.data = serde_json::from_str(&data)?;
    Ok(event)
}
