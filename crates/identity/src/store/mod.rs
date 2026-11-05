//! Persistent credentials: hashed scoped keys with a display prefix, revocable
//! logins with expiry, and single-use pairing codes with attempt limits, kept
//! in the identity store's own SQLite file (see `database`).
//!
//! Every check-and-write runs in one `BEGIN IMMEDIATE` transaction that reads
//! the clock after taking the write lock, so writers are serialized across
//! threads, connections and processes. Nothing is cached: a revocation applies
//! to every authentication that starts after it commits.
//!
//! The store resolves presented text to an [`authz::Principal`] and records
//! lifecycle changes. It does not decide who may issue, list or revoke; that is
//! [`authz::authorize`] together with the identity authority, which is user
//! decision C1. All token formats and state names are provisional wire names.
//!
//! Known limitations:
//! - Plaintext lives in ordinary heap memory until dropped and is not zeroed.
//! - A pairing code carries 60 secret bits. Online guessing is bounded by
//!   [`PAIRING_MAX_ATTEMPTS`]. Offline, a database copied while a code is live
//!   holds its digest, but searching 60 bits is impractical within the code's
//!   lifetime of at most [`PAIRING_MAX_LIFETIME`]. An expired code's digest
//!   stays until the next pairing write, when it can no longer be claimed.
//! - Approval-window and pairing durations are unknown in the specification,
//!   so callers supply them; pairings are capped by provisional maximums.
//!
//! [`authz::Principal`]: crate::authz::Principal
//! [`authz::authorize`]: crate::authz::authorize
mod bearer;
mod clock;
pub(crate) mod database;
mod error;
mod pairing;
mod secret;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, Transaction, TransactionBehavior};

pub use bearer::{CredentialRecord, IssuedCredential};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::StoreError;
pub use pairing::{
    IssuedPairing, PAIRING_MAX_ATTEMPTS, PAIRING_MAX_LIFETIME, PairingRecord, PairingState,
};
pub use secret::SecretText;

use crate::authz::Scope;
use error::Storage;

/// The identity database's file name inside the Foundry state directory.
pub const IDENTITY_DATABASE: &str = "identity.sqlite3";

struct Shared {
    clock: Arc<dyn Clock>,
    connection: Mutex<Connection>,
}

/// Cloning shares one connection. Separate `open` calls on the same file use
/// separate connections and still serialize through SQLite.
#[derive(Clone)]
pub struct CredentialStore {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialStore")
            .finish_non_exhaustive()
    }
}

impl CredentialStore {
    /// Opens or creates the identity database at `path`.
    pub fn open(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        Ok(Self::with_connection(database::open(path.as_ref())?, clock))
    }

    /// Opens or creates [`IDENTITY_DATABASE`] in the Foundry state directory.
    pub fn open_in(
        state_directory: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        Self::open(state_directory.as_ref().join(IDENTITY_DATABASE), clock)
    }

    /// A private in-memory database; nothing outlives the store.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
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

    fn write<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>, u64) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.transaction(TransactionBehavior::Immediate, operation)
    }

    fn read<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>, u64) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.transaction(TransactionBehavior::Deferred, operation)
    }

    /// Runs `operation` in one transaction and commits it only on success. The
    /// clock is read after `BEGIN`, so an immediate transaction already holds
    /// the write lock when it reads the time.
    fn transaction<T>(
        &self,
        behavior: TransactionBehavior,
        operation: impl FnOnce(&Transaction<'_>, u64) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self
            .shared
            .connection
            .lock()
            .map_err(|_| StoreError::unavailable("connection lock poisoned"))?;
        let transaction = connection.transaction_with_behavior(behavior).storage()?;
        let now_ms = self.shared.clock.now_ms();
        let value = operation(&transaction, now_ms)?;
        transaction.commit().storage()?;
        Ok(value)
    }
}

fn required(value: &str, field: &'static str) -> Result<(), StoreError> {
    if value.is_empty() {
        return Err(StoreError::InvalidRequest { field });
    }
    Ok(())
}

fn scopes_to_sql(scopes: &BTreeSet<Scope>) -> String {
    scopes
        .iter()
        .map(|scope| scope.name())
        .collect::<Vec<_>>()
        .join(" ")
}

fn scopes_from_sql(text: &str) -> Result<BTreeSet<Scope>, StoreError> {
    text.split_whitespace()
        .map(|name| Scope::from_name(name).ok_or(StoreError::unavailable("malformed stored scope")))
        .collect()
}
