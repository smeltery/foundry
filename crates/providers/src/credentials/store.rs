//! Credential lifecycle operations over the credential file. Every query is
//! keyed by the caller's workspace first, and every write runs in one
//! `BEGIN IMMEDIATE` transaction that reads, authorizes and writes.
use super::database::{self, Storage, to_sql};
use super::rows::{self, connected_by, free_account_key, slots, validate_config};
use super::{
    AccountCheck, AccountKey, Caller, CheckCause, CredentialError, CredentialEvent, CredentialSlot,
    CredentialState, CredentialValidator, EvidenceBasis, ProviderEvidence, Secret,
};
use crate::catalog::{AuthKind, ConfiguredCredentials, ProviderCatalog, ProviderId};
use foundry_identity::authz::{PrincipalId, WorkspaceId};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Largest accepted non-secret config value, in bytes.
pub const MAX_CONFIG_VALUE_BYTES: usize = 1024;

/// Milliseconds since the Unix epoch.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Debug)]
pub struct ConfigureRequest {
    pub provider_id: ProviderId,
    /// `None` allocates the lowest free `account-N` slot, so only a request
    /// naming its slot is idempotent.
    pub account_key: Option<AccountKey>,
    pub secret: Secret,
    /// Non-secret settings; every name must be a config field the provider's
    /// descriptor declares, and every required field must be present.
    pub config: BTreeMap<String, String>,
}

/// The configured slot, by name only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configured {
    pub provider_id: ProviderId,
    pub account_key: AccountKey,
    pub kind: AuthKind,
    pub state: CredentialState,
    pub config_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub store_available: bool,
    pub configure: bool,
    pub delete: bool,
    pub validate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialList {
    /// Ordered by provider ID, then account key.
    pub items: Vec<CredentialSlot>,
    pub readiness: Readiness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    pub provider_id: ProviderId,
    pub account_keys: Vec<AccountKey>,
    /// Whether the WAL was truncated as well. The deletion is committed
    /// either way; a concurrent reader can delay the truncation until a later
    /// write's checkpoint.
    pub secret_purged: bool,
}

/// One saved key on its way into the adapter's private inputs.
#[derive(Debug)]
pub struct CredentialHandoff {
    provider_id: ProviderId,
    secret: Secret,
}

impl CredentialHandoff {
    /// The provider and value `AdapterCredential::ApiKey` takes.
    pub fn into_adapter_parts(self) -> (String, String) {
        (
            self.provider_id.as_str().to_owned(),
            self.secret.into_inner(),
        )
    }
}

pub struct CredentialStore {
    connection: Mutex<Connection>,
    clock: Clock,
}

impl CredentialStore {
    pub fn open(path: &Path) -> Result<Self, CredentialError> {
        Self::open_with_clock(path, Arc::new(wall_clock_ms))
    }

    pub fn open_with_clock(path: &Path, clock: Clock) -> Result<Self, CredentialError> {
        Ok(Self {
            connection: Mutex::new(database::open(path)?),
            clock,
        })
    }

    /// Stores or replaces an API key. Replacing a slot someone else connected
    /// needs an owner or administrator; the replacer then owns the slot.
    pub fn configure(
        &self,
        caller: &Caller,
        catalog: &ProviderCatalog,
        request: ConfigureRequest,
    ) -> Result<Configured, CredentialError> {
        let provider = catalog
            .provider(&request.provider_id)
            .ok_or(CredentialError::NotFound { what: "provider" })?;
        if !provider.accepts(AuthKind::ApiKey) {
            return Err(CredentialError::UnsupportedProvider);
        }
        validate_config(&provider.config_fields, &request.config)?;
        if !caller.may_write() {
            return Err(CredentialError::Forbidden);
        }
        let now = to_sql((self.clock)())?;
        let slot = (caller.workspace.as_str(), request.provider_id.as_str());
        let account_key = self.write(|transaction| {
            let account_key = match request.account_key {
                Some(key) => key,
                None => free_account_key(transaction, slot)?,
            };
            if let Some(owner) = connected_by(transaction, slot, &account_key)?
                && !caller.may_manage(&owner)
            {
                return Err(CredentialError::Forbidden);
            }
            let row = rows::Upsert {
                slot,
                account_key: &account_key,
                secret: &request.secret,
                connected_by: &caller.principal,
                now,
                config: &request.config,
            };
            rows::upsert(transaction, row)?;
            Ok(account_key)
        })?;
        // A replaced key's old pages are zeroed; drop them from the WAL too.
        // The write is committed, so a delayed checkpoint is not a failure.
        self.purge();
        Ok(Configured {
            provider_id: request.provider_id,
            account_key,
            kind: AuthKind::ApiKey,
            state: CredentialState::Configured,
            config_names: request.config.into_keys().collect(),
        })
    }

    /// Every slot in the caller's workspace, with state but no secret.
    pub fn list(&self, caller: &Caller) -> Result<CredentialList, CredentialError> {
        let items = self.read(|connection| slots(connection, &caller.workspace, None))?;
        let write = caller.may_write();
        Ok(CredentialList {
            items,
            readiness: Readiness {
                store_available: true,
                configure: write,
                delete: write,
                validate: write,
            },
        })
    }

    /// Deletes one slot, or every slot of the provider when `account_key` is
    /// `None`. All targeted slots must be the caller's to manage, otherwise
    /// nothing is deleted. A running attempt is not cancelled; its next
    /// handoff finds no credential.
    pub fn delete(
        &self,
        caller: &Caller,
        provider_id: &ProviderId,
        account_key: Option<&AccountKey>,
    ) -> Result<Deleted, CredentialError> {
        if !caller.may_write() {
            return Err(CredentialError::Forbidden);
        }
        let account_keys = self.write(|transaction| {
            let targets: Vec<CredentialSlot> =
                slots(transaction, &caller.workspace, Some(provider_id))?
                    .into_iter()
                    .filter(|slot| account_key.is_none_or(|key| *key == slot.account_key))
                    .collect();
            if targets.is_empty() {
                return Err(CredentialError::NotFound { what: "credential" });
            }
            if targets
                .iter()
                .any(|slot| !caller.may_manage(&slot.connected_by))
            {
                return Err(CredentialError::Forbidden);
            }
            for slot in &targets {
                transaction
                    .execute(
                        "DELETE FROM credential_slots
                         WHERE workspace = ?1 AND provider_id = ?2 AND account_key = ?3",
                        params![
                            caller.workspace.as_str(),
                            provider_id.as_str(),
                            slot.account_key.as_str()
                        ],
                    )
                    .storage()?;
            }
            Ok(targets.into_iter().map(|slot| slot.account_key).collect())
        })?;
        Ok(Deleted {
            provider_id: provider_id.clone(),
            account_keys,
            secret_purged: self.purge(),
        })
    }

    /// Checks one saved slot with provider metadata only. A missing slot is
    /// `connected: false` and an unreadable file `connected: None`, not
    /// errors. Nothing is written.
    pub fn check_account(
        &self,
        caller: &Caller,
        catalog: &ProviderCatalog,
        validator: &impl CredentialValidator,
        provider_id: &ProviderId,
        account_key: &AccountKey,
    ) -> Result<AccountCheck, CredentialError> {
        let provider = catalog
            .provider(provider_id)
            .ok_or(CredentialError::NotFound { what: "provider" })?;
        let saved = EvidenceBasis::SavedCredential;
        let (connected, evidence) = match self.load(&caller.workspace, provider_id, account_key) {
            Err(_) => (
                None,
                ProviderEvidence::unverified(saved, CheckCause::StorageUnavailable),
            ),
            Ok(None) => (
                Some(false),
                ProviderEvidence::unverified(saved, CheckCause::NotConnected),
            ),
            Ok(Some((owner, _, _))) if !caller.may_manage(&owner) => {
                return Err(CredentialError::Forbidden);
            }
            Ok(Some((_, _, secret))) => (Some(true), validator.check(provider, &secret)),
        };
        Ok(AccountCheck {
            connected,
            evidence,
            observed_at_ms: (self.clock)(),
        })
    }

    /// Applies a provider-side event from the state table. Events outside the
    /// table are `conflict` and change nothing.
    pub fn record(
        &self,
        workspace: &WorkspaceId,
        provider_id: &ProviderId,
        account_key: &AccountKey,
        event: CredentialEvent,
    ) -> Result<CredentialState, CredentialError> {
        let now = to_sql((self.clock)())?;
        self.write(|transaction| {
            let slot = slots(transaction, workspace, Some(provider_id))?
                .into_iter()
                .find(|slot| slot.account_key == *account_key)
                .ok_or(CredentialError::NotFound { what: "credential" })?;
            let next = slot.state.after(event).ok_or(CredentialError::Conflict {
                what: "credential state",
            })?;
            transaction
                .execute(
                    "UPDATE credential_slots SET state = ?4, failure_code = ?5, failed_at_ms = ?6
                     WHERE workspace = ?1 AND provider_id = ?2 AND account_key = ?3",
                    params![
                        workspace.as_str(),
                        provider_id.as_str(),
                        account_key.as_str(),
                        next.name(),
                        event.failure_code(),
                        now,
                    ],
                )
                .storage()?;
            Ok(next)
        })
    }

    /// Releases a configured key for the private-input handoff. Only callers
    /// who may manage the slot receive it; any other state is `conflict`.
    pub fn handoff(
        &self,
        caller: &Caller,
        provider_id: &ProviderId,
        account_key: &AccountKey,
    ) -> Result<CredentialHandoff, CredentialError> {
        let (owner, state, secret) = self
            .load(&caller.workspace, provider_id, account_key)?
            .ok_or(CredentialError::NotFound { what: "credential" })?;
        if !caller.may_manage(&owner) {
            return Err(CredentialError::Forbidden);
        }
        if state != CredentialState::Configured {
            return Err(CredentialError::Conflict {
                what: "credential state",
            });
        }
        Ok(CredentialHandoff {
            provider_id: provider_id.clone(),
            secret,
        })
    }

    /// The provider and kind pairs with a configured credential in one
    /// workspace, for catalog availability.
    pub fn availability(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<ConfiguredCredentials, CredentialError> {
        let items = self.read(|connection| slots(connection, workspace, None))?;
        Ok(ConfiguredCredentials::new(
            items
                .into_iter()
                .filter(|slot| slot.state == CredentialState::Configured)
                .map(|slot| (slot.provider_id, slot.kind)),
        ))
    }

    fn load(
        &self,
        workspace: &WorkspaceId,
        provider_id: &ProviderId,
        account_key: &AccountKey,
    ) -> Result<Option<(PrincipalId, CredentialState, Secret)>, CredentialError> {
        self.read(|connection| rows::load(connection, workspace, provider_id, account_key))
    }

    fn read<T>(
        &self,
        body: impl FnOnce(&Connection) -> Result<T, CredentialError>,
    ) -> Result<T, CredentialError> {
        let connection = self.lock()?;
        body(&connection)
    }

    fn write<T>(
        &self,
        body: impl FnOnce(&Transaction<'_>) -> Result<T, CredentialError>,
    ) -> Result<T, CredentialError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .storage()?;
        let value = body(&transaction)?;
        transaction.commit().storage()?;
        Ok(value)
    }

    fn purge(&self) -> bool {
        self.lock()
            .and_then(|connection| database::purge_wal(&connection))
            .unwrap_or(false)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CredentialError> {
        self.connection
            .lock()
            .map_err(|_| CredentialError::unavailable("credential store poisoned"))
    }
}

/// Validates a key against the provider without saving it.
pub fn validate(
    catalog: &ProviderCatalog,
    validator: &impl CredentialValidator,
    provider_id: &ProviderId,
    secret: &Secret,
) -> Result<super::ValidationOutcome, CredentialError> {
    let provider = catalog
        .provider(provider_id)
        .ok_or(CredentialError::NotFound { what: "provider" })?;
    if !provider.accepts(AuthKind::ApiKey) {
        return Err(CredentialError::UnsupportedProvider);
    }
    Ok(validator.validate(provider, secret))
}

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
