//! Versioned storage for the membership projection, never a full team document.
#[path = "persistence_document.rs"]
mod document;
#[path = "persistence_storage.rs"]
pub(crate) mod storage;
use crate::{
    WireProfile,
    membership::{MembershipSnapshot, MembershipState, MergeReport},
};
use iroh::EndpointId;
use rusqlite::{Connection, TransactionBehavior, params};
use std::{fmt, path::Path, time::Duration};

pub const MAX_MEMBERSHIP_BYTES: usize = 4 * 1024 * 1024;
pub const MEMBERSHIP_DATABASE: &str = "membership.sqlite3";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceError {
    Invalid,
    UnsafeStorage,
    Storage,
    Busy,
    Schema,
    Corrupt,
    IdentityMismatch,
    AlreadyInitialized,
    Uninitialized,
    Conflict,
    TooLarge,
    RevisionOverflow,
    Unsupported,
}
impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid membership persistence input",
            Self::UnsafeStorage => "membership storage protection failed",
            Self::Storage => "membership storage operation failed",
            Self::Busy => "membership storage is busy",
            Self::Schema => "unsupported membership storage schema",
            Self::Corrupt => "invalid stored membership",
            Self::IdentityMismatch => "membership identity or profile mismatch",
            Self::AlreadyInitialized => "membership storage already initialized",
            Self::Uninitialized => "membership storage is not initialized",
            Self::Conflict => "membership revision conflict",
            Self::TooLarge => "membership payload exceeds limit",
            Self::RevisionOverflow => "membership revision exhausted",
            Self::Unsupported => "membership storage unsupported on this platform",
        })
    }
}
impl std::error::Error for PersistenceError {}
impl From<rusqlite::Error> for PersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                Self::Busy
            }
            _ => Self::Storage,
        }
    }
}
pub type Result<T> = std::result::Result<T, PersistenceError>;
pub struct StoredMembership {
    pub revision: u64,
    pub state: MembershipState,
}
pub struct MembershipStore {
    connection: Connection,
    protection: storage::Protection,
}
impl MembershipStore {
    /// Fixed filename in an existing absolute, private directory. Busy timeout
    /// must be positive and at most 60 seconds; it does not bound kernel I/O.
    pub fn open(root: &Path, busy_timeout: Duration) -> Result<Self> {
        if !root.is_absolute() || busy_timeout.is_zero() || busy_timeout > Duration::from_secs(60) {
            return Err(PersistenceError::Invalid);
        }
        let protection = storage::Protection::open(root)?;
        let mut connection = Connection::open_with_flags(
            protection.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(busy_timeout)?;
        protection.check()?;
        storage::schema(&mut connection)?;
        let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if mode != "wal" {
            return Err(PersistenceError::Storage);
        }
        connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF;")?;
        protection.sync()?;
        Ok(Self {
            connection,
            protection,
        })
    }
    pub fn initialize(
        &mut self,
        state: &MembershipState,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<StoredMembership> {
        self.protection.check()?;
        let bytes = document::encode(state.snapshot(), profile)?;
        let verified = document::validate(state.snapshot(), profile, now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        storage::verify_schema(&tx)?;
        if document::read(&tx)?.is_some() {
            return Err(PersistenceError::AlreadyInitialized);
        }
        if tx.execute(
            "INSERT INTO projection (id,revision,payload) VALUES (1,1,?1)",
            [bytes],
        )? != 1
        {
            return Err(PersistenceError::Storage);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(StoredMembership {
            revision: 1,
            state: verified,
        })
    }
    pub fn load(
        &mut self,
        expected_team: &str,
        expected_founder: EndpointId,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<Option<StoredMembership>> {
        self.protection.check()?;
        let tx = self.connection.transaction()?;
        storage::verify_schema(&tx)?;
        let result = document::read(&tx)?
            .map(|(revision, doc)| -> Result<StoredMembership> {
                doc.matches(expected_team, expected_founder, profile)?;
                Ok(StoredMembership {
                    revision,
                    state: document::validate(&doc.snapshot, &doc.profile, now_ms)?,
                })
            })
            .transpose()?;
        tx.commit()?;
        Ok(result)
    }
    pub fn merge(
        &mut self,
        expected_revision: u64,
        authenticated_peer: EndpointId,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<(StoredMembership, MergeReport)> {
        self.protection.check()?;
        // Bound untrusted input before signature/merge work as well as the final document.
        document::encode(incoming, profile)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        storage::verify_schema(&tx)?;
        let (revision, doc) = document::read(&tx)?.ok_or(PersistenceError::Uninitialized)?;
        if revision != expected_revision {
            return Err(PersistenceError::Conflict);
        }
        doc.matches(&incoming.team, incoming.founder, profile)?;
        let mut state = document::validate(&doc.snapshot, &doc.profile, now_ms)?;
        let report = state
            .merge_from(authenticated_peer, incoming, profile, now_ms)
            .map_err(|_| PersistenceError::Invalid)?;
        let next = if report.changed {
            let next = revision
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(PersistenceError::RevisionOverflow)?;
            let bytes = document::encode(state.snapshot(), profile)?;
            document::validate(state.snapshot(), profile, now_ms)?;
            if tx.execute(
                "UPDATE projection SET revision=?1,payload=?2 WHERE id=1 AND revision=?3",
                params![next as i64, bytes, revision as i64],
            )? != 1
            {
                return Err(PersistenceError::Storage);
            }
            next
        } else {
            revision
        };
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok((
            StoredMembership {
                revision: next,
                state,
            },
            report,
        ))
    }
}
