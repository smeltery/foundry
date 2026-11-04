//! Attempt-scaled delivery cursors and immutable display positions for events.
//!
//! Both are derived facts stored beside `events.sequence`, which is never
//! rewritten because it feeds checkpoint cut hashes. Rows written before schema
//! v12 (or by a writer that left the columns NULL) are stamped with the same
//! rule the first time a write or delivery read touches their session:
//!
//! - Walk the session's rows by `sequence`. The first row gets attempt ordinal 1.
//!   A row keeps the previous row's ordinal while its attempt is unchanged and
//!   takes the previous ordinal plus one when the attempt differs.
//! - A row's position is the position of the earliest row with the same event
//!   ID, which for that earliest row is its own `sequence`.
//!
//! Values already stored are kept. The delivery sequence is
//! `(attempt_ordinal - 1) * 2^32 + sequence`, so it increases with commit order
//! and equals the stored sequence during a session's first attempt run.
use crate::{Result, Store, conflict, read_session};
use foundry_contracts::{ContractError, Event, Lifecycle, Session};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::{collections::HashMap, fmt, time::Duration};

pub const DEFAULT_EVENT_PAGE: u32 = 200;
pub const MAX_EVENT_PAGE: u32 = 1000;
pub const MAX_EVENT_WAIT_MS: u32 = 20_000;
const OFFSET_BITS: u32 = 32;
const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1;

/// Exclusive delivery cursor. `Start` precedes the first event; its wire form is -1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DeliveryCursor {
    #[default]
    Start,
    After(u64),
}
impl DeliveryCursor {
    /// Accepts -1 or any unsigned 64-bit value; anything else is out of range.
    pub fn from_wire(value: i128) -> Result<Self> {
        match value {
            -1 => Ok(Self::Start),
            _ => u64::try_from(value)
                .map(Self::After)
                .map_err(|_| ContractError::Invalid("after_seq is out of range".into()).into()),
        }
    }
    pub fn to_wire(self) -> i128 {
        match self {
            Self::Start => -1,
            Self::After(value) => value.into(),
        }
    }
    /// The (attempt counter, offset) pair a row must exceed; `Start` admits every row.
    fn bound(self) -> (i64, i64) {
        match self {
            Self::Start => (-1, 0),
            Self::After(value) => ((value >> OFFSET_BITS) as i64, (value & OFFSET_MASK) as i64),
        }
    }
}
impl fmt::Display for DeliveryCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_wire())
    }
}

/// A delivery read. Absent limit and wait take their defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventRead {
    pub after_seq: DeliveryCursor,
    pub limit: Option<u32>,
    pub wait_ms: Option<u32>,
}
impl EventRead {
    pub fn page_size(&self) -> Result<u32> {
        let limit = self.limit.unwrap_or(DEFAULT_EVENT_PAGE);
        if !(1..=MAX_EVENT_PAGE).contains(&limit) {
            return Err(ContractError::Invalid("event limit must be 1 to 1000".into()).into());
        }
        Ok(limit)
    }
    pub fn wait(&self) -> Result<Duration> {
        let wait = self.wait_ms.unwrap_or(0);
        if wait > MAX_EVENT_WAIT_MS {
            return Err(ContractError::Invalid("event wait must be 0 to 20000 ms".into()).into());
        }
        Ok(Duration::from_millis(wait.into()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeliveredEvent {
    pub delivery_seq: u64,
    pub attempt_ordinal: u64,
    /// Sequence of the event ID's first revision; later revisions keep it.
    pub position: u64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventPage {
    pub session: Session,
    pub events: Vec<DeliveredEvent>,
    /// Resume cursor: the last delivered sequence, or the request cursor when empty.
    pub next_seq: DeliveryCursor,
}
impl EventPage {
    /// Waiting can yield new events only while the session may still produce them.
    pub fn may_grow(&self) -> bool {
        matches!(
            self.session.lifecycle,
            Lifecycle::Queued | Lifecycle::Running
        )
    }
}

/// Keeps the highest delivered copy of each event ID, ordered by position.
pub fn transcript(events: &[DeliveredEvent]) -> Vec<&DeliveredEvent> {
    let mut latest: HashMap<&str, &DeliveredEvent> = HashMap::new();
    for event in events {
        let slot = latest.entry(&event.event.id).or_insert(event);
        if event.delivery_seq > slot.delivery_seq {
            *slot = event;
        }
    }
    let mut ordered: Vec<_> = latest.into_values().collect();
    ordered.sort_by_key(|event| (event.position, event.delivery_seq));
    ordered
}

impl Store {
    /// Events after an exclusive delivery cursor, stamping unstamped rows first.
    pub fn deliver_events(&mut self, session_id: &str, read: &EventRead) -> Result<EventPage> {
        let limit = read.page_size()?;
        read.wait()?;
        read_session(&self.connection, session_id)?;
        if has_unstamped(&self.connection, session_id)? {
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            stamp_session(&tx, session_id)?;
            tx.commit()?;
        }
        let (counter, offset) = read.after_seq.bound();
        let mut statement = self.connection.prepare(
            "SELECT id,attempt_id,sequence,revision,kind,data,attempt_ordinal,position FROM events
             WHERE session_id=?1 AND (attempt_ordinal-1>?2 OR (attempt_ordinal-1=?2 AND sequence>?3))
             ORDER BY sequence LIMIT ?4",
        )?;
        let rows = statement.query_map(params![session_id, counter, offset, limit], |row| {
            Ok((
                Event {
                    id: row.get(0)?,
                    session_id: session_id.into(),
                    attempt_id: row.get(1)?,
                    sequence: row.get(2)?,
                    revision: row.get(3)?,
                    kind: row.get(4)?,
                    data: Value::Null,
                },
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut events = Vec::new();
        let mut next_seq = read.after_seq;
        for row in rows {
            let (mut event, data, ordinal, position) = row?;
            event.data = serde_json::from_str(&data)?;
            let delivery_seq = delivery_seq(ordinal, event.sequence)?;
            if DeliveryCursor::After(delivery_seq) <= next_seq {
                return Err(conflict("stored event delivery order is inconsistent"));
            }
            next_seq = DeliveryCursor::After(delivery_seq);
            events.push(DeliveredEvent {
                delivery_seq,
                attempt_ordinal: ordinal as u64,
                position: positive(position)?,
                event,
            });
        }
        drop(statement);
        Ok(EventPage {
            session: read_session(&self.connection, session_id)?,
            events,
            next_seq,
        })
    }

    /// Changes whenever another connection, in this or another process, commits.
    pub fn commit_marker(&self) -> Result<i64> {
        Ok(self
            .connection
            .pragma_query_value(None, "data_version", |row| row.get(0))?)
    }
}

fn has_unstamped(connection: &Connection, session_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND (attempt_ordinal IS NULL OR position IS NULL))",
        [session_id],
        |row| row.get(0),
    )?)
}

/// Stamps every unstamped row of a session by the module rule. Event writers
/// call this after inserting, inside the same transaction.
pub(crate) fn stamp_session(tx: &Connection, session_id: &str) -> Result<()> {
    let first: Option<i64> = tx.query_row(
        "SELECT MIN(sequence) FROM events WHERE session_id=?1 AND (attempt_ordinal IS NULL OR position IS NULL)",
        [session_id],
        |row| row.get(0),
    )?;
    let Some(first) = first else {
        return Ok(());
    };
    // Every earlier row is stamped, so the latest one seeds the walk.
    let mut previous: Option<(String, i64)> = tx
        .query_row(
            "SELECT attempt_id,attempt_ordinal FROM events WHERE session_id=?1 AND sequence<?2
             ORDER BY sequence DESC LIMIT 1",
            params![session_id, first],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let mut rows = tx.prepare(
        "SELECT sequence,id,attempt_id,attempt_ordinal,position FROM events
         WHERE session_id=?1 AND sequence>=?2 ORDER BY sequence",
    )?;
    let rows = rows
        .query_map(params![session_id, first], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut earliest = tx.prepare(
        "SELECT COALESCE(position,sequence) FROM events WHERE session_id=?1 AND id=?2
         ORDER BY sequence LIMIT 1",
    )?;
    for (sequence, id, attempt, stored_ordinal, stored_position) in rows {
        let derived = match &previous {
            None => 1,
            Some((prior, ordinal)) if *prior == attempt => *ordinal,
            Some((_, ordinal)) => ordinal
                .checked_add(1)
                .ok_or_else(|| conflict("event attempt ordinal exhausted"))?,
        };
        let ordinal = stored_ordinal.unwrap_or(derived);
        if previous.as_ref().is_some_and(|(_, prior)| ordinal < *prior) {
            return Err(conflict("stored event attempt ordinal decreased"));
        }
        delivery_seq(ordinal, sequence as u64)?;
        let position = match stored_position {
            Some(position) => position,
            None => earliest.query_row(params![session_id, id], |row| row.get(0))?,
        };
        if stored_ordinal.is_none() || stored_position.is_none() {
            tx.execute(
                "UPDATE events SET attempt_ordinal=?1, position=?2 WHERE session_id=?3 AND sequence=?4",
                params![ordinal, position, session_id, sequence],
            )?;
        }
        previous = Some((attempt, ordinal));
    }
    Ok(())
}

fn delivery_seq(ordinal: i64, sequence: u64) -> Result<u64> {
    let counter = positive(ordinal)? - 1;
    if counter > OFFSET_MASK || sequence == 0 || sequence > OFFSET_MASK {
        return Err(conflict("event delivery sequence exceeds its range"));
    }
    Ok((counter << OFFSET_BITS) | sequence)
}

fn positive(value: i64) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| conflict("stored event delivery field is not positive"))
}
