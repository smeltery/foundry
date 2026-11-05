//! Local organization operations: workspace create, read, rename and delete;
//! member invite, removal and role changes; settings namespaces; single-use
//! workspace create and retire; and the append-only audit log. They persist in
//! the identity database (schema version 2, see `store::database`).
//!
//! Every operation runs in one transaction that reads a fresh snapshot of the
//! committed workspace state, hands it to [`authz::authorize`], and only then
//! applies the operation rules. A mutation and its audit entry commit together;
//! a denial or failed rule writes nothing. Mutations take the write lock first
//! (`BEGIN IMMEDIATE`), so concurrent changes from separate connections or
//! processes are serialized and each one decides against the state the
//! previous one committed.
//!
//! The caller passes a principal already resolved from its credential, for
//! example by the credential store, and applies throttling before calling.
//!
//! Held, not implemented here:
//! - Bootstrapping the first workspace, accepting an invitation and delivering
//!   invitation notices belong to the identity authority (decision C1);
//!   invitations are recorded and report zero deliveries.
//! - Personal workspaces, settlement-blocked deletion, single-use budgets and
//!   active limits (C1, C4). Deletion completes in one step.
//! - A single-use workspace's machine token, live work and machines are owned
//!   elsewhere, so retirement reports zero cancelled runs and revoked machines.
//! - Matrix cells the specification leaves unknown, including
//!   `single_use.create`, `settings.list` and `settings.get`, are denied by the
//!   decision core for every caller.
//!
//! All operation, field, state and cause names are provisional wire names
//! pending NIC-36 C2 and NIC-34 D1/D6.
//!
//! [`authz::authorize`]: crate::authz::authorize
mod audit;
mod error;
#[cfg(test)]
mod held_tests;
mod member;
mod settings;
mod single_use;
mod snapshot;
mod workspace;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

pub use audit::{
    AUDIT_PAGE_DEFAULT, AUDIT_PAGE_MAX, AuditCursor, AuditEntry, AuditPage, AuditQuery,
};
pub use error::{OrgCause, OrgError};
pub use member::{Invitation, InviteOutcome, MemberList, MemberRecord};
pub use settings::{SettingsCatalog, SettingsNamespace};
pub use single_use::{
    LIFETIME_DEFAULT_MINUTES, RetireCause, RetireOutcome, SingleUseCreated, SingleUseRecord,
    SingleUseRequest,
};
pub use workspace::WorkspaceRecord;

use crate::authz::{Access, OperationRequest, Principal, WorkspaceId, authorize};
use crate::store::database::{self, to_sql};
use crate::store::{Clock, IDENTITY_DATABASE};
use error::Storage;

struct Shared {
    clock: Arc<dyn Clock>,
    connection: Mutex<Connection>,
}

/// Cloning shares one connection. Separate `open` calls on the same file use
/// separate connections and still serialize through SQLite.
#[derive(Clone)]
pub struct OrgStore {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for OrgStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OrgStore").finish_non_exhaustive()
    }
}

/// An authorized request inside its open transaction.
struct Authorized<'a> {
    transaction: &'a Transaction<'a>,
    now_ms: u64,
    caller: &'a Principal,
    access: Access,
}

impl OrgStore {
    /// Opens or creates the identity database at `path`, applying the
    /// organization schema step to a version 1 file.
    pub fn open(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self, OrgError> {
        Ok(Self::with_connection(database::open(path.as_ref())?, clock))
    }

    /// Opens or creates [`IDENTITY_DATABASE`] in the Foundry state directory.
    pub fn open_in(
        state_directory: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, OrgError> {
        Self::open(state_directory.as_ref().join(IDENTITY_DATABASE), clock)
    }

    /// A private in-memory database; nothing outlives the store.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, OrgError> {
        Ok(Self::with_connection(database::open_in_memory()?, clock))
    }

    fn with_connection(connection: Connection, clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                connection: Mutex::new(connection),
            }),
        }
    }

    /// Authorizes and applies a mutation under the write lock.
    fn write<T>(
        &self,
        caller: &Principal,
        request: OperationRequest,
        operation: impl FnOnce(&Authorized<'_>) -> Result<T, OrgError>,
    ) -> Result<T, OrgError> {
        self.authorized(TransactionBehavior::Immediate, caller, request, operation)
    }

    /// Authorizes and answers a read from one consistent snapshot.
    fn read<T>(
        &self,
        caller: &Principal,
        request: OperationRequest,
        operation: impl FnOnce(&Authorized<'_>) -> Result<T, OrgError>,
    ) -> Result<T, OrgError> {
        self.authorized(TransactionBehavior::Deferred, caller, request, operation)
    }

    /// Loads the snapshot, decides, and runs `operation` in one transaction
    /// that commits only on success. The clock is read after `BEGIN`, so an
    /// immediate transaction already holds the write lock when it reads it.
    fn authorized<T>(
        &self,
        behavior: TransactionBehavior,
        caller: &Principal,
        request: OperationRequest,
        operation: impl FnOnce(&Authorized<'_>) -> Result<T, OrgError>,
    ) -> Result<T, OrgError> {
        let mut connection = self
            .shared
            .connection
            .lock()
            .map_err(|_| OrgError::unavailable("connection lock poisoned"))?;
        let transaction = connection.transaction_with_behavior(behavior).storage()?;
        let now_ms = self.shared.clock.now_ms();
        let organization = snapshot::load(&transaction, caller, &request, now_ms)?;
        let access = authorize(Some(caller), &organization, &request)?;
        let value = operation(&Authorized {
            transaction: &transaction,
            now_ms,
            caller,
            access,
        })?;
        transaction.commit().storage()?;
        Ok(value)
    }
}

/// What one audit entry records about a mutation.
struct Change<'a> {
    action: &'static str,
    target_kind: &'static str,
    target: &'a str,
    label: Option<&'a str>,
    details: String,
}

impl Authorized<'_> {
    /// Appends the audit entry for a mutation in the same transaction. The
    /// actor's email is copied as it is at the time of the change.
    fn record(&self, workspace: &WorkspaceId, change: Change<'_>) -> Result<(), OrgError> {
        let actor = self.caller.id.as_str();
        self.transaction
            .execute(
                "INSERT INTO org_audit (workspace, action, actor, actor_email, target_kind,
                     target, label, details, created_ms)
                 VALUES (?1, ?2, ?3,
                     (SELECT email FROM org_members WHERE workspace = ?1 AND principal = ?3),
                     ?4, ?5, ?6, ?7, ?8)",
                params![
                    workspace.as_str(),
                    change.action,
                    actor,
                    change.target_kind,
                    change.target,
                    change.label,
                    change.details,
                    to_sql(self.now_ms)?,
                ],
            )
            .storage()?;
        Ok(())
    }
}

/// Text that is not blank after trimming, trimmed.
fn nonblank<'a>(text: &'a str, field: &'static str) -> Result<&'a str, OrgError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OrgError::invalid(field));
    }
    Ok(trimmed)
}

/// A new opaque identifier: `tag` and 20 random lowercase hex digits.
fn new_id(tag: &str) -> Result<String, OrgError> {
    let mut bytes = [0u8; 10];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrgError::unavailable(format!("random source: {error}")))?;
    let digits: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{tag}_{digits}"))
}
