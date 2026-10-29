//! Durable local mailbox transitions. Callers authorize recipients and sessions.
//! An acknowledgement records adapter delivery, never distributed exactly-once.
use crate::persistence::{
    PersistenceError,
    storage::{OpenMode, Protection},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, time::Duration};
#[path = "mail_store/batch.rs"]
mod batch;
#[path = "mail_store/incremental.rs"]
mod incremental;
pub(crate) use incremental::InboxBatch;
#[path = "mail_store/payload.rs"]
mod payload;
pub use batch::MAX_INBOX_MESSAGES;
#[path = "mail_store/polling.rs"]
mod polling;
pub use payload::PayloadEncoding;
pub use polling::PollRead;
#[path = "mail_store/retry.rs"]
mod retry;
#[path = "mail_store/schema.rs"]
mod schema;
#[path = "mail_store/transitions.rs"]
mod transitions;

pub type Result<T> = std::result::Result<T, MailError>;
#[derive(Debug)]
pub enum MailError {
    InvalidInput,
    InvalidPayload,
    UnsupportedPayload,
    Conflict,
    Capacity,
    Missing,
    StaleAttempt,
    WrongRecipient,
    InvalidTransition,
    Schema,
    Output,
    Storage(rusqlite::Error),
    Protection(PersistenceError),
}
impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not expose SQL parameters or message contents.
        f.write_str(match self {
            Self::InvalidInput => "invalid mail input",
            Self::InvalidPayload => "invalid stored mail payload",
            Self::UnsupportedPayload => "mail payload needs an unsupported handler",
            Self::Conflict => "message ID conflict",
            Self::Capacity => "mail capacity reached",
            Self::Missing => "message missing",
            Self::WrongRecipient => "attempt belongs to another recipient",
            Self::StaleAttempt => "delivery attempt is stale",
            Self::InvalidTransition => "invalid mail transition",
            Self::Output => "inbox output failed",
            Self::Schema => "unsupported mail schema",
            Self::Storage(_) => "mail database failure",
            Self::Protection(_) => "mail storage protection failure",
        })
    }
}
impl std::error::Error for MailError {}
impl From<rusqlite::Error> for MailError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<PersistenceError> for MailError {
    fn from(e: PersistenceError) -> Self {
        Self::Protection(e)
    }
}

/// Immutable delivery preferences; these flags do not assert adapter capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryIntent {
    pub wake: bool,
    pub notify: bool,
}
impl Default for DeliveryIntent {
    fn default() -> Self {
        Self {
            wake: true,
            notify: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub body: Vec<u8>,
    pub encoding: PayloadEncoding,
    pub reply_to: Option<String>,
    pub intent: DeliveryIntent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Held,
    Rejected,
    Delivered,
    Consumed,
}
impl State {
    fn code(self) -> i64 {
        match self {
            Self::Pending => 0,
            Self::Held => 1,
            Self::Rejected => 2,
            Self::Delivered => 3,
            Self::Consumed => 4,
        }
    }
    fn decode(v: i64) -> rusqlite::Result<Self> {
        match v {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Held),
            2 => Ok(Self::Rejected),
            3 => Ok(Self::Delivered),
            4 => Ok(Self::Consumed),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    Release,
    Reject,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Insert {
    Created,
    Duplicate,
}
#[derive(Debug)]
pub struct StoredMail {
    pub message: Message,
    pub state: State,
    pub retry: RetryStatus,
}
/// Opaque token bound to a message and session. Persisted sequence never reused.
#[derive(Clone, Debug)]
enum AttemptKind {
    Push,
    Peek,
}
#[derive(Clone, Debug)]
pub struct Attempt {
    id: i64,
    message_id: String,
    session: String,
    expires_ms: i64,
    kind: AttemptKind,
}
impl Attempt {
    pub fn id(&self) -> i64 {
        self.id
    }
    /// Peek tokens are connection-scoped and have no time deadline.
    pub fn deadline_ms(&self) -> Option<i64> {
        match self.kind {
            AttemptKind::Push => Some(self.expires_ms),
            AttemptKind::Peek => None,
        }
    }
    pub fn expires_ms(&self) -> i64 {
        self.expires_ms
    }
}
#[derive(Debug)]
pub struct Delivery {
    pub mail: Message,
    pub attempt: Attempt,
}

pub struct MailStore {
    connection: Connection,
    protection: Protection,
}
/// Automatic claims stop after this many attempts, including lost leases.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 8;
pub const RETRY_BASE_MS: i64 = 1_000;
pub const RETRY_CAP_MS: i64 = 60_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Durable scheduling state. `next_ms` is the earliest next automatic claim;
/// an active lease must also expire. Exhaustion leaves the message unread.
/// Callers supply nonnegative epoch milliseconds; clock rollback delays recovery.
pub struct RetryStatus {
    pub attempts: u32,
    pub next_ms: i64,
}
impl RetryStatus {
    pub fn exhausted(self) -> bool {
        self.attempts >= MAX_DELIVERY_ATTEMPTS
    }
}
pub const MAX_MAIL_ROWS: usize = 10_000;
pub const MAX_MAIL_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_MAIL_BODY: usize = 256 * 1024;
pub const MAX_MAIL_IDENTIFIER: usize = 256;
pub const MAX_POLL_SCOPES: usize = 1024;
const MAX_ROWS: i64 = MAX_MAIL_ROWS as i64;
const MAX_BYTES: i64 = MAX_MAIL_BODY_BYTES as i64;
fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_MAIL_IDENTIFIER || value.chars().any(char::is_control)
    {
        return Err(MailError::InvalidInput);
    }
    Ok(())
}
fn timestamp(now: i64) -> Result<()> {
    if now < 0 {
        Err(MailError::InvalidInput)
    } else {
        Ok(())
    }
}
fn read_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        sender: row.get(1)?,
        recipient: row.get(2)?,
        body: row.get(3)?,
        encoding: PayloadEncoding::decode(row.get(7)?)?,
        reply_to: row.get(4)?,
        intent: DeliveryIntent {
            wake: row.get(5)?,
            notify: row.get(6)?,
        },
    })
}
fn read_stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMail> {
    Ok(StoredMail {
        message: read_message(row)?,
        state: State::decode(row.get(8)?)?,
        retry: RetryStatus {
            attempts: row.get(9)?,
            next_ms: row.get(10)?,
        },
    })
}
impl MailStore {
    /// Uses a private directory. Exact v2 schema migrates transactionally to v3 by
    /// adding read metadata and peek tokens; old terminal states and every row survive.
    pub fn open(root: &Path) -> Result<Self> {
        if !root.is_absolute() {
            return Err(MailError::InvalidInput);
        }
        let protection = Protection::open_named(root, "mail.sqlite3", OpenMode::CreateOrOpen)?;
        let mut connection = Connection::open_with_flags(
            protection.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        protection.check()?;
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA journal_mode=DELETE;",
        )?;
        schema::initialize(&mut connection)?;
        incremental::storage_budget(&connection)?;
        protection.sync()?;
        Ok(Self {
            connection,
            protection,
        })
    }
    /// Immutable identity includes contents, reply target and delivery intent. Exact retries ignore current state.
    pub fn insert(&mut self, message: &Message, initial: State) -> Result<Insert> {
        identifier(&message.id)?;
        identifier(&message.sender)?;
        identifier(&message.recipient)?;
        if let Some(reply) = &message.reply_to {
            identifier(reply)?;
        }
        if message.body.len() > MAX_MAIL_BODY
            || !matches!(initial, State::Pending | State::Held | State::Rejected)
        {
            return Err(MailError::InvalidInput);
        }
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = tx
            .query_row(
                "SELECT id,sender,recipient,body,reply_to,wake,notify,encoding FROM mail WHERE id=?1",
                [&message.id],
                read_message,
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior != *message {
                return Err(MailError::Conflict);
            }
            message.validate_payload()?;
            return Ok(Insert::Duplicate);
        }
        message.validate_payload()?;
        let (count, bytes): (i64, i64) = tx.query_row(
            "SELECT count(*),coalesce(sum(length(body)),0) FROM mail",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if count >= MAX_ROWS || bytes + message.body.len() as i64 > MAX_BYTES {
            return Err(MailError::Capacity);
        }
        tx.execute(
            "INSERT INTO mail(id,sender,recipient,body,reply_to,wake,notify,encoding,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                message.id,
                message.sender,
                message.recipient,
                message.body,
                message.reply_to, message.intent.wake, message.intent.notify, message.encoding.code(),
                initial.code()
            ],
        )?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(Insert::Created)
    }
    pub fn get(&self, id: &str) -> Result<StoredMail> {
        identifier(id)?;
        self.protection.check()?;
        self.connection
            .query_row(
                "SELECT id,sender,recipient,body,reply_to,wake,notify,encoding,CASE WHEN state=0 AND read_ms IS NOT NULL THEN 4 ELSE state END,tries,next_ms FROM mail WHERE id=?1",
                [id],
                read_stored,
            )
            .optional()?
            .ok_or(MailError::Missing)
    }
    /// Peek never creates or alters an attempt. Leased rows remain unread.
    pub fn peek(&self, recipient: &str, limit: usize) -> Result<Vec<Message>> {
        identifier(recipient)?;
        if limit == 0 || limit > 100 {
            return Err(MailError::InvalidInput);
        }
        self.protection.check()?;
        let mut query = self.connection.prepare("SELECT id,sender,recipient,body,reply_to,wake,notify,encoding FROM mail WHERE recipient=?1 AND state=0 AND read_ms IS NULL ORDER BY rowid LIMIT ?2")?;
        Ok(query
            .query_map(params![recipient, limit as i64], read_message)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn unread(&self, recipient: &str) -> Result<u64> {
        identifier(recipient)?;
        self.protection.check()?;
        Ok(self.connection.query_row(
            "SELECT count(*) FROM mail WHERE recipient=?1 AND state=0 AND read_ms IS NULL",
            [recipient],
            |r| r.get(0),
        )?)
    }
    /// Sender history retains pending, held and terminal rows; no implicit pruning.
    pub fn outbox(&self, sender: &str, limit: usize) -> Result<Vec<StoredMail>> {
        identifier(sender)?;
        if limit == 0 || limit > 100 {
            return Err(MailError::InvalidInput);
        }
        self.protection.check()?;
        let mut query = self.connection.prepare("SELECT id,sender,recipient,body,reply_to,wake,notify,encoding,CASE WHEN state=0 AND read_ms IS NOT NULL THEN 4 ELSE state END,tries,next_ms FROM mail WHERE sender=?1 ORDER BY rowid LIMIT ?2")?;
        Ok(query
            .query_map(params![sender, limit as i64], read_stored)?
            .collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(all(test, unix))]
#[path = "mail_store/batch_tests.rs"]
mod batch_tests;
