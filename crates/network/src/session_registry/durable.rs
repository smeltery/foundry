use super::*;
use crate::persistence::{
    PersistenceError,
    storage::{OpenMode, Protection},
};
use rusqlite::{Connection, TransactionBehavior, params};
use std::{path::Path, time::Duration};

#[path = "document.rs"]
mod document;
#[cfg(test)]
#[path = "durable_tests.rs"]
mod tests;

pub const SESSION_DATABASE: &str = "sessions.sqlite3";
const TABLE: &str = "CREATE TABLE registry (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(revision>0), payload BLOB NOT NULL)";
const APPLICATION: i64 = 0x46535247;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableError {
    Registry(RegistryError),
    Storage,
    UnsafeStorage,
    Busy,
    Schema,
    Corrupt,
    Conflict,
    Indeterminate,
    ReloadRequired,
    Unsupported,
}
impl std::fmt::Display for DurableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session storage: {self:?}")
    }
}
impl std::error::Error for DurableError {}
impl From<RegistryError> for DurableError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}
impl From<PersistenceError> for DurableError {
    fn from(value: PersistenceError) -> Self {
        match value {
            PersistenceError::UnsafeStorage => Self::UnsafeStorage,
            PersistenceError::Busy => Self::Busy,
            PersistenceError::Unsupported => Self::Unsupported,
            _ => Self::Storage,
        }
    }
}
impl From<rusqlite::Error> for DurableError {
    fn from(value: rusqlite::Error) -> Self {
        PersistenceError::from(value).into()
    }
}
type StoredResult<T> = std::result::Result<T, DurableError>;

/// Unix protected SQLite, synchronous FULL with rollback journaling. Unsupported
/// platforms fail explicitly through Protection. Contains plaintext secrets in a
/// private database, not encryption. No Debug or snapshot logging interface.
///
/// Caller ordering: serialize daemon operations; transact stages an isolated copy,
/// commits and syncs it, then publishes live state and returns. Only after Ok may
/// the caller reply/advertise/perform delivery. The closure must have no external
/// effects and must not leak its staged credential. On a storage failure the
/// wrapper blocks reads and writes: drop and reopen with current folder policy.
/// A lost commit result is Indeterminate, never permission to retry registration
/// blindly. Reopen retains durable identity but clears every connection binding.
/// The daemon still needs its single-instance lock; revision CAS rejects stale
/// cooperating writers but does not synchronize another process's live readers.
pub struct DurableRegistry {
    connection: Connection,
    protection: Protection,
    live: Option<SessionRegistry>,
    revision: i64,
}
impl DurableRegistry {
    pub fn open(root: &Path, policy: FolderPolicy, timeout: Duration) -> StoredResult<Self> {
        if !root.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(DurableError::Storage);
        }
        let protection = Protection::open_named(root, SESSION_DATABASE, OpenMode::CreateOrOpen)?;
        let mut connection = Connection::open_with_flags(
            protection.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(timeout)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;")?;
        let mode: String = connection.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        if mode != "delete" {
            return Err(DurableError::Storage);
        }
        protection.check()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let count: i64 = tx.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        if app == 0 && version == 0 && count == 0 {
            tx.execute_batch(TABLE)?;
            tx.pragma_update(None, "application_id", APPLICATION)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.execute(
                "INSERT INTO registry VALUES(1,1,?1)",
                [b"{\"sessions\":[]}".to_vec()],
            )?;
        }
        verify(&tx)?;
        let (revision, payload) = read(&tx)?;
        let live = document::decode(&payload, policy)?;
        protection.check()?;
        tx.commit()?;
        protection.sync_database()?;
        Ok(Self {
            connection,
            protection,
            live: Some(live),
            revision,
        })
    }

    pub fn registry(&self) -> StoredResult<&SessionRegistry> {
        self.protection.check()?;
        self.live.as_ref().ok_or(DurableError::ReloadRequired)
    }

    /// Includes durable status changes; associations are deliberately not encoded.
    /// Invalid registry operations leave the current live state untouched.
    pub fn transact<T>(
        &mut self,
        operation: impl FnOnce(&mut SessionRegistry) -> Result<T>,
    ) -> StoredResult<T> {
        let mut staged = self.registry()?.clone();
        let result = operation(&mut staged)?;
        let payload = document::encode(&staged)?;
        if let Err(error) = self.commit(payload) {
            self.live = None;
            return Err(error);
        }
        self.live = Some(staged);
        Ok(result)
    }

    fn commit(&mut self, payload: Vec<u8>) -> StoredResult<()> {
        self.protection.check()?;
        let next = self.revision.checked_add(1).ok_or(DurableError::Conflict)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify(&tx)?;
        if tx.execute(
            "UPDATE registry SET revision=?1,payload=?2 WHERE id=1 AND revision=?3",
            params![next, payload, self.revision],
        )? != 1
        {
            return Err(DurableError::Conflict);
        }
        self.protection.check()?;
        tx.commit().map_err(|_| DurableError::Indeterminate)?;
        self.protection
            .sync_database()
            .map_err(|_| DurableError::Indeterminate)?;
        self.revision = next;
        Ok(())
    }
}
fn verify(connection: &Connection) -> StoredResult<()> {
    let app: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
    if app != APPLICATION || version != 1 || count != 1 {
        return Err(DurableError::Schema);
    }
    let valid: bool = connection.query_row(
        "SELECT type='table' AND name='registry' AND sql=?1 FROM sqlite_schema",
        [TABLE],
        |r| r.get(0),
    )?;
    if !valid {
        return Err(DurableError::Schema);
    }
    Ok(())
}
fn read(connection: &Connection) -> StoredResult<(i64, Vec<u8>)> {
    let (count, length): (i64, i64) = connection.query_row(
        "SELECT count(*),coalesce(max(length(payload)),0) FROM registry",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count != 1 || length < 1 || length > document::MAX_BYTES as i64 {
        return Err(DurableError::Corrupt);
    }
    let (id, revision, payload): (i64, i64, Vec<u8>) =
        connection.query_row("SELECT id,revision,payload FROM registry", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    if id != 1 || revision < 1 {
        return Err(DurableError::Corrupt);
    }
    Ok((revision, payload))
}
