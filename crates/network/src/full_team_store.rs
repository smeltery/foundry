//! Local identity-sealed full documents. No remote-history or rollback guarantee.
//! Typed transitions, atomic code intents, local removal and checked live-file scrub.
#[path = "full_team_store_checkpoint.rs"]
mod checkpoint;
#[path = "full_team_store_code_migration.rs"]
mod code_migration;
#[path = "full_team_store_input.rs"]
mod input;
#[path = "full_team_store_intent_recovery.rs"]
mod intent_recovery;
#[path = "full_team_store_intent_types.rs"]
mod intent_types;
#[path = "full_team_store_intents.rs"]
mod intents;
#[cfg(all(test, unix))]
#[path = "full_team_store_relay_tests.rs"]
mod relay_tests;
pub use intent_types::{
    CodeIntentId, CodeLease, ControlEffect, IssuedCodeInvite, PendingCodeIntent,
};
#[path = "full_team_store_join.rs"]
mod join;
#[path = "full_team_store_legacy.rs"]
mod legacy;
#[path = "full_team_store_owner.rs"]
mod owner;
pub use join::{CodeJoinTrust, JoinTrust, PendingJoin};
#[path = "full_team_store_policy.rs"]
mod policy;
#[path = "full_team_store_scrub.rs"]
mod scrub;
pub use policy::StorePolicy;
pub(crate) use policy::VerifiedLocalSnapshot;
#[path = "full_team_store_transitions.rs"]
mod transitions;
#[path = "full_team_store_validation.rs"]
mod validation;
use crate::persistence::{
    PersistenceError,
    storage::{OpenMode, Protection},
};
use crate::{
    LocalIdentity, WireProfile,
    proof::{ProofConfig, ProofVerifier},
    team::TeamDocument,
    team_state::{Formation, SystemEntropy, TeamState},
};
use checkpoint::{Checkpoint, StoredState, digest};
use rusqlite::{Connection, TransactionBehavior};
use std::{fmt, path::Path, sync::Arc, time::Duration};
pub use transitions::{
    CommitOutcome, LoadedStatus, PendingTransition, RemovalReceipt, ScrubFailure, ScrubStatus,
};

pub const FULL_TEAM_DATABASE: &str = "full-team.sqlite3";
pub const FULL_TEAM_LOCK: &str = "full-team.lock";
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Invalid,
    Storage,
    UnsafeStorage,
    Busy,
    Schema,
    Corrupt,
    Binding,
    Conflict,
    Cancelled,
    TooLarge,
    Unsupported,
    RevisionOverflow,
    Indeterminate,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid full team storage input",
            Self::Storage => "full team storage failed",
            Self::UnsafeStorage => "full team storage protection failed",
            Self::Busy => "full team storage busy",
            Self::Schema => "unsupported full team schema",
            Self::Corrupt => "invalid full team checkpoint",
            Self::Binding => "full team storage binding mismatch",
            Self::Conflict => "full team checkpoint conflict",
            Self::Cancelled => "full team commit cancelled before writing",
            Self::TooLarge => "full team checkpoint exceeds limit",
            Self::RevisionOverflow => "full team revision exhausted",
            Self::Indeterminate => "full team commit outcome requires reload",
            Self::Unsupported => "full team operation unsupported",
        })
    }
}
impl std::error::Error for StoreError {}
impl From<PersistenceError> for StoreError {
    fn from(error: PersistenceError) -> Self {
        match error {
            PersistenceError::UnsafeStorage => Self::UnsafeStorage,
            PersistenceError::Busy => Self::Busy,
            PersistenceError::Unsupported => Self::Unsupported,
            _ => Self::Storage,
        }
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        PersistenceError::from(error).into()
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;
struct StoreInstance {
    incarnation: [u8; 32],
}
/// No public constructor or deserializer; retained state is privileged, not log data.
pub struct LoadedTeam {
    instance: Arc<StoreInstance>,
    checkpoint: Checkpoint,
    digest: [u8; 32],
}
impl LoadedTeam {
    pub fn revision(&self) -> u64 {
        self.checkpoint.revision
    }
    pub fn document(&self) -> Option<&TeamDocument> {
        self.checkpoint.document()
    }
}
pub struct PendingFormation {
    instance: Arc<StoreInstance>,
    base: [u8; 32],
    checkpoint: Checkpoint,
}
pub struct FullTeamStore {
    connection: Connection,
    protection: Protection,
    lock: Protection,
    identity: Arc<LocalIdentity>,
    policy: StorePolicy,
    instance: Arc<StoreInstance>,
    timeout: Duration,
    #[cfg(test)]
    fail_sync: bool,
    #[cfg(test)]
    crash_after_commit: bool,
    #[cfg(test)]
    fail_scrub_sync: bool,
}
impl FullTeamStore {
    /// Explicitly create a new database; existing files are never replaced.
    pub fn create_new(
        root: &Path,
        identity: Arc<LocalIdentity>,
        policy: StorePolicy,
        timeout: Duration,
    ) -> Result<Self> {
        Self::open(root, identity, policy, timeout, true, None)
    }
    /// Missing/corrupt storage is an error, never an implicit formation/reset.
    pub fn open_existing(
        root: &Path,
        identity: Arc<LocalIdentity>,
        policy: StorePolicy,
        timeout: Duration,
    ) -> Result<Self> {
        Self::open(root, identity, policy, timeout, false, None)
    }
    /// Explicit lossless migration; ordinary open leaves older versions untouched.
    pub fn migrate_v1(
        root: &Path,
        identity: Arc<LocalIdentity>,
        policy: StorePolicy,
        timeout: Duration,
        now_ms: i64,
    ) -> Result<Self> {
        Self::open(root, identity, policy, timeout, false, Some(now_ms))
    }
    fn open(
        root: &Path,
        identity: Arc<LocalIdentity>,
        policy: StorePolicy,
        timeout: Duration,
        create: bool,
        migrate: Option<i64>,
    ) -> Result<Self> {
        if !root.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(StoreError::Invalid);
        }
        let protection = Protection::open_named(
            root,
            FULL_TEAM_DATABASE,
            if create {
                OpenMode::Create
            } else {
                OpenMode::Existing
            },
        )?;
        // Separate inode: locking the SQLite file can conflict with its own VFS locks.
        let lock = Protection::open_named(root, FULL_TEAM_LOCK, OpenMode::CreateOrOpen)?;
        let guard = lock.lock(timeout)?;
        protection.check()?;
        let mut connection = Connection::open_with_flags(
            protection.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(timeout)?;
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;",
        )?;
        if let Some(now) = migrate {
            legacy::migrate(&mut connection, &identity, &policy, now, &protection)?;
        }
        let incarnation = (|| -> Result<[u8; 32]> {
            let incarnation = if create {
                let mut incarnation = [0; 32];
                getrandom::fill(&mut incarnation).map_err(|_| StoreError::Storage)?;
                let mut empty =
                    Checkpoint::empty(incarnation, identity.endpoint_id(), policy.digest);
                empty.configure(&policy);
                checkpoint::create(&mut connection, &empty, &identity)?;
                incarnation
            } else {
                let (stored, _) = checkpoint::read(&connection, &identity, &policy)?;
                stored.incarnation
            };
            let journal: String =
                connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
            if journal != "wal" {
                return Err(StoreError::Storage);
            }
            #[cfg(test)]
            if migrate.is_some() && legacy::FAIL_SYNC.with(|flag| flag.replace(false)) {
                return Err(StoreError::Indeterminate);
            }
            protection.sync()?;
            Ok(incarnation)
        })()
        .map_err(|error| {
            if migrate.is_some() {
                StoreError::Indeterminate
            } else {
                error
            }
        })?;
        drop(guard);
        Ok(Self {
            connection,
            protection,
            lock,
            identity,
            policy,
            instance: Arc::new(StoreInstance { incarnation }),
            timeout,
            #[cfg(test)]
            fail_sync: false,
            #[cfg(test)]
            crash_after_commit: false,
            #[cfg(test)]
            fail_scrub_sync: false,
        })
    }
    pub fn load(&mut self, now_ms: i64) -> Result<LoadedTeam> {
        match self.load_status(now_ms)? {
            LoadedStatus::Empty(state) | LoadedStatus::Active(state) => Ok(state),
            LoadedStatus::Removed(_) => Err(StoreError::Unsupported),
        }
    }
    pub fn load_status(&mut self, now_ms: i64) -> Result<LoadedStatus> {
        let _guard = self.lock.lock(self.timeout)?;
        self.protection.check()?;
        let tx = self.connection.transaction()?;
        let (checkpoint, digest) = checkpoint::read(&tx, &self.identity, &self.policy)?;
        if checkpoint.incarnation != self.instance.incarnation {
            return Err(StoreError::Binding);
        }
        validation::validate(&checkpoint, &self.policy, now_ms)?;
        self.protection.check()?;
        tx.commit()?;
        let state = LoadedTeam {
            instance: self.instance.clone(),
            checkpoint,
            digest,
        };
        Ok(match &state.checkpoint.state {
            StoredState::Empty => LoadedStatus::Empty(state),
            StoredState::Active { .. } => LoadedStatus::Active(state),
            StoredState::Removed { .. } => LoadedStatus::Removed(RemovalReceipt {
                instance: self.instance.clone(),
                revision: state.revision(),
                digest,
            }),
        })
    }
    pub fn prepare_formation(
        &self,
        base: &LoadedTeam,
        fields: Formation<'_>,
        now_ms: i64,
    ) -> Result<PendingFormation> {
        self.prepare_formation_with_relay(base, fields, None, now_ms)
    }
    pub fn commit_formation(&mut self, pending: PendingFormation) -> Result<LoadedTeam> {
        self.commit_formation_when(pending, || true)
    }
    fn commit_formation_when(
        &mut self,
        pending: PendingFormation,
        begin_write: impl FnOnce() -> bool,
    ) -> Result<LoadedTeam> {
        if !Arc::ptr_eq(&self.instance, &pending.instance) {
            return Err(StoreError::Binding);
        }
        let _guard = self.lock.lock(self.timeout)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (base, base_digest) = checkpoint::read(&tx, &self.identity, &self.policy)?;
        if base.incarnation != self.instance.incarnation
            || base_digest != pending.base
            || base.revision != 0
            || base.document().is_some()
        {
            return Err(StoreError::Conflict);
        }
        if !begin_write() {
            return Err(StoreError::Cancelled);
        }
        let digest = checkpoint::write(&tx, &pending.checkpoint, &self.identity, false)?;
        self.protection.check()?;
        tx.commit().map_err(|_| StoreError::Indeterminate)?;
        self.sync_committed()?;
        Ok(LoadedTeam {
            instance: self.instance.clone(),
            checkpoint: pending.checkpoint,
            digest,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "../tests/full_team_store_support/internal.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "../tests/full_team_store_support/transitions.rs"]
mod transition_tests;

#[cfg(all(test, unix))]
#[path = "../tests/full_team_store_support/migration.rs"]
mod migration_tests;

#[cfg(all(test, unix))]
#[path = "../tests/full_team_store_support/clock.rs"]
mod clock_tests;

#[cfg(all(test, unix))]
#[path = "full_team_store_intent_tests.rs"]
mod intent_tests;
