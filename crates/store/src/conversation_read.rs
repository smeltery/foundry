use crate::{Result, Store, read_session};
use foundry_contracts::{ContractError, Event, Message, MessageState};
use rusqlite::params;
use serde_json::Value;

/// One conversation-visible event at its highest stored revision.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRecord {
    /// Sequence of the event ID's first revision; later revisions keep this position.
    pub position: u64,
    pub event: Event,
    /// The delivered instruction named by a delivery event.
    pub message: Option<Message>,
}

// Replies, settled results and delivery receipts. Every other kind (readiness, tool
// requests, failures, snapshots) is diagnostic and stays in the event log only.
// A delivery receipt is shown only once its instruction is durably delivered.
const CONVERSATION: &str = "
    WITH latest AS (
        SELECT id, MIN(sequence) AS position, MAX(revision) AS revision FROM events
        WHERE session_id=?1
          AND kind IN ('text','adapter_settled','message_accepted','continue_accepted')
        GROUP BY id
    )
    SELECT l.position, e.id, e.attempt_id, e.sequence, e.revision, e.kind, e.data,
           m.id, m.sequence, m.text, m.attempt_id
    FROM latest l
    JOIN events e ON e.session_id=?1 AND e.id=l.id AND e.revision=l.revision
    LEFT JOIN messages m ON e.kind IN ('message_accepted','continue_accepted')
        AND m.session_id=?1 AND m.id=json_extract(e.data,'$.message_id')
        AND m.state='delivered'
    WHERE l.position>?2 AND (e.kind IN ('text','adapter_settled') OR m.id IS NOT NULL)
    ORDER BY l.position LIMIT ?3";

impl Store {
    /// Conversation-visible events after an exclusive position cursor, one per event ID.
    pub fn conversation_events(
        &self,
        session_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<ConversationRecord>> {
        read_session(&self.connection, session_id)?;
        let after = i64::try_from(after).map_err(|_| {
            ContractError::Invalid("conversation cursor exceeds durable integer range".into())
        })?;
        let mut statement = self.connection.prepare(CONVERSATION)?;
        let rows = statement.query_map(params![session_id, after, limit], |row| {
            let message = match row.get::<_, Option<String>>(7)? {
                Some(id) => Some(Message {
                    id,
                    session_id: session_id.into(),
                    sequence: row.get(8)?,
                    text: row.get(9)?,
                    state: MessageState::Delivered,
                    attempt_id: row.get(10)?,
                }),
                None => None,
            };
            let event = Event {
                id: row.get(1)?,
                session_id: session_id.into(),
                attempt_id: row.get(2)?,
                sequence: row.get(3)?,
                revision: row.get(4)?,
                kind: row.get(5)?,
                data: Value::Null,
            };
            Ok((row.get(0)?, event, row.get::<_, String>(6)?, message))
        })?;
        rows.map(|row| {
            let (position, mut event, data, message) = row?;
            event.data = serde_json::from_str(&data)?;
            Ok(ConversationRecord {
                position,
                event,
                message,
            })
        })
        .collect()
    }
}
