//! Row queries for the credential file. The secret column is selected only by
//! [`load`].
use super::database::{Storage, from_sql};
use super::slot::{failure_from_code, kind_from_name};
use super::store::MAX_CONFIG_VALUE_BYTES;
use super::{AccountKey, CredentialError, CredentialSlot, CredentialState, Secret};
use crate::catalog::{ConfigField, ProviderId};
use foundry_identity::authz::{PrincipalId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate_config(
    fields: &[ConfigField],
    config: &BTreeMap<String, String>,
) -> Result<(), CredentialError> {
    let mut declared = BTreeSet::new();
    for field in fields {
        if field.required && !config.contains_key(&field.name) {
            return Err(CredentialError::invalid("config"));
        }
        declared.insert(field.name.as_str());
    }
    let malformed = |value: &String| {
        value.is_empty()
            || value.len() > MAX_CONFIG_VALUE_BYTES
            || value.chars().any(char::is_control)
    };
    if config
        .iter()
        .any(|(name, value)| !declared.contains(name.as_str()) || malformed(value))
    {
        return Err(CredentialError::invalid("config"));
    }
    Ok(())
}

pub(super) fn free_account_key(
    connection: &Connection,
    (workspace, provider_id): (&str, &str),
) -> Result<AccountKey, CredentialError> {
    let mut statement = connection
        .prepare(
            "SELECT account_key FROM credential_slots WHERE workspace = ?1 AND provider_id = ?2",
        )
        .storage()?;
    let taken: BTreeSet<String> = statement
        .query_map(params![workspace, provider_id], |row| row.get(0))
        .storage()?
        .collect::<Result<_, _>>()
        .storage()?;
    let free = (1..=taken.len() + 1)
        .map(|index| format!("account-{index}"))
        .find(|key| !taken.contains(key))
        .ok_or(CredentialError::unavailable("no free account key"))?;
    AccountKey::new(&free)
}

pub(super) fn connected_by(
    connection: &Connection,
    (workspace, provider_id): (&str, &str),
    account_key: &AccountKey,
) -> Result<Option<PrincipalId>, CredentialError> {
    connection
        .query_row(
            "SELECT connected_by FROM credential_slots
             WHERE workspace = ?1 AND provider_id = ?2 AND account_key = ?3",
            params![workspace, provider_id, account_key.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .storage()
        .map(|owner| owner.map(PrincipalId::new))
}

pub(super) struct Upsert<'a> {
    /// Workspace and provider ID.
    pub slot: (&'a str, &'a str),
    pub account_key: &'a AccountKey,
    pub secret: &'a Secret,
    pub connected_by: &'a PrincipalId,
    pub now: i64,
    pub config: &'a BTreeMap<String, String>,
}

/// Inserts or replaces a configured API-key slot and its whole config. Any
/// earlier failure is cleared; the creation time is kept on replace.
pub(super) fn upsert(connection: &Connection, row: Upsert<'_>) -> Result<(), CredentialError> {
    let (workspace, provider_id) = row.slot;
    let key = row.account_key.as_str();
    connection
        .execute(
            "INSERT INTO credential_slots (workspace, provider_id, account_key, kind, state,
                 secret, connected_by, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'api_key', 'configured', ?4, ?5, ?6, ?6)
             ON CONFLICT DO UPDATE SET kind = excluded.kind, state = excluded.state,
                 secret = excluded.secret, connected_by = excluded.connected_by,
                 updated_at_ms = excluded.updated_at_ms, failure_code = NULL,
                 failed_at_ms = NULL",
            params![
                workspace,
                provider_id,
                key,
                row.secret.expose().as_bytes(),
                row.connected_by.as_str(),
                row.now,
            ],
        )
        .storage()?;
    connection
        .execute(
            "DELETE FROM credential_config
             WHERE workspace = ?1 AND provider_id = ?2 AND account_key = ?3",
            params![workspace, provider_id, key],
        )
        .storage()?;
    for (name, value) in row.config {
        connection
            .execute(
                "INSERT INTO credential_config VALUES (?1, ?2, ?3, ?4, ?5)",
                params![workspace, provider_id, key, name, value],
            )
            .storage()?;
    }
    Ok(())
}

/// Slots in one workspace, optionally of one provider, with their config
/// names.
pub(super) fn slots(
    connection: &Connection,
    workspace: &WorkspaceId,
    provider_id: Option<&ProviderId>,
) -> Result<Vec<CredentialSlot>, CredentialError> {
    let mut names: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT provider_id, account_key, name FROM credential_config
             WHERE workspace = ?1 AND (?2 IS NULL OR provider_id = ?2) ORDER BY name",
        )
        .storage()?;
    let provider = provider_id.map(ProviderId::as_str);
    let rows = statement
        .query_map(params![workspace.as_str(), provider], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
            ))
        })
        .storage()?;
    for row in rows {
        let (provider, account, name) = row.storage()?;
        names.entry((provider, account)).or_default().push(name);
    }
    let mut statement = connection
        .prepare(
            "SELECT provider_id, account_key, kind, state, connected_by, created_at_ms,
                    updated_at_ms, failure_code, failed_at_ms
             FROM credential_slots WHERE workspace = ?1 AND (?2 IS NULL OR provider_id = ?2)
             ORDER BY provider_id, account_key",
        )
        .storage()?;
    let rows = statement
        .query_map(params![workspace.as_str(), provider], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                (row.get::<_, String>(2)?, row.get::<_, String>(3)?),
                row.get::<_, String>(4)?,
                (row.get::<_, i64>(5)?, row.get::<_, i64>(6)?),
                (
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ),
            ))
        })
        .storage()?;
    let corrupt = || CredentialError::unavailable("malformed stored slot");
    let mut items = Vec::new();
    for row in rows {
        let (key, (kind, state), owner, (created, updated), failure) = row.storage()?;
        let failure = match failure {
            (Some(code), Some(at)) => {
                Some(failure_from_code(&code, from_sql(at)?).ok_or_else(corrupt)?)
            }
            (None, None) => None,
            _ => return Err(corrupt()),
        };
        items.push(CredentialSlot {
            provider_id: ProviderId::new(&key.0).map_err(|_| corrupt())?,
            account_key: AccountKey::new(&key.1).map_err(|_| corrupt())?,
            kind: kind_from_name(&kind).ok_or_else(corrupt)?,
            state: parse_state(&state)?,
            config_names: names.remove(&key).unwrap_or_default(),
            connected_by: PrincipalId::new(owner),
            created_at_ms: from_sql(created)?,
            updated_at_ms: from_sql(updated)?,
            failure,
        });
    }
    Ok(items)
}

fn parse_state(name: &str) -> Result<CredentialState, CredentialError> {
    CredentialState::from_name(name).ok_or(CredentialError::unavailable("malformed stored slot"))
}

/// The owner, state and secret of one slot.
pub(super) fn load(
    connection: &Connection,
    workspace: &WorkspaceId,
    provider_id: &ProviderId,
    account_key: &AccountKey,
) -> Result<Option<(PrincipalId, CredentialState, Secret)>, CredentialError> {
    let row = connection
        .query_row(
            "SELECT connected_by, state, secret FROM credential_slots
             WHERE workspace = ?1 AND provider_id = ?2 AND account_key = ?3",
            params![
                workspace.as_str(),
                provider_id.as_str(),
                account_key.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                ))
            },
        )
        .optional()
        .storage()?;
    row.map(|(owner, state, secret)| {
        Ok((
            PrincipalId::new(owner),
            parse_state(&state)?,
            Secret::from_stored(secret)?,
        ))
    })
    .transpose()
}
