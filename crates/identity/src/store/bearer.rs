//! Scoped keys and logins. Both are presented as `<tag>_<id>_<secret>`, where
//! `<tag>_<id>` is the public display prefix that lists and revocations use,
//! and only a digest of the secret part is stored. Keys stay valid until
//! revoked; logins also expire.
use std::collections::BTreeSet;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::database::{expiry, from_sql, to_sql};
use super::error::{Storage, StoreError};
use super::secret::{self, SecretText};
use super::{CredentialStore, required, scopes_from_sql, scopes_to_sql};
use crate::authz::{
    Credential, CredentialKind, CredentialState, Principal, PrincipalId, Scope, WorkspaceId,
};

const PREFIX_SYMBOLS: usize = 10;
/// 52 symbols of five bits each: 260 random bits.
const SECRET_SYMBOLS: usize = 52;

/// A stored key or login as it may be shown: never its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRecord {
    /// Public display prefix, such as `key_ABCDEFGHJK`.
    pub prefix: String,
    pub kind: CredentialKind,
    pub principal: PrincipalId,
    pub workspace: WorkspaceId,
    pub scopes: BTreeSet<Scope>,
    /// `Active`, `Revoked` or `Expired`, as of the read.
    pub state: CredentialState,
    pub created_ms: u64,
    pub expires_ms: Option<u64>,
}

/// A newly issued credential. `secret` is the only copy of the presentable
/// text; the store cannot show it again.
#[derive(Debug, Clone)]
pub struct IssuedCredential {
    pub record: CredentialRecord,
    pub secret: SecretText,
}

/// Provisional tags. Admin grants and machine tokens are not stored here.
fn tag(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::LoginGrant => "login",
        _ => "key",
    }
}

fn kind_from_tag(tag: &str) -> Option<CredentialKind> {
    match tag {
        "key" => Some(CredentialKind::ScopedKey),
        "login" => Some(CredentialKind::LoginGrant),
        _ => None,
    }
}

/// Splits presented text into its prefix and secret, or `None` when malformed.
fn parse(presented: &str) -> Option<(&str, &str)> {
    let (prefix, secret_part) = presented.rsplit_once('_')?;
    let (tag, id) = prefix.split_once('_')?;
    let well_formed = kind_from_tag(tag).is_some()
        && secret::is_symbols(id, PREFIX_SYMBOLS)
        && secret::is_symbols(secret_part, SECRET_SYMBOLS);
    well_formed.then_some((prefix, secret_part))
}

const COLUMNS: &str =
    "prefix, kind, principal, workspace, scopes, created_ms, expires_ms, revoked_ms, secret_digest";

fn record_from_row(
    row: &rusqlite::Row<'_>,
    now_ms: u64,
) -> Result<(CredentialRecord, Vec<u8>), StoreError> {
    let kind: String = row.get(1).storage()?;
    let expires_ms = row
        .get::<_, Option<i64>>(6)
        .storage()?
        .map(from_sql)
        .transpose()?;
    let revoked = row.get::<_, Option<i64>>(7).storage()?.is_some();
    let state = if revoked {
        CredentialState::Revoked
    } else if expires_ms.is_some_and(|expires| now_ms >= expires) {
        CredentialState::Expired
    } else {
        CredentialState::Active
    };
    let record = CredentialRecord {
        prefix: row.get(0).storage()?,
        kind: CredentialKind::from_name(&kind)
            .ok_or(StoreError::unavailable("malformed stored kind"))?,
        principal: PrincipalId::new(row.get::<_, String>(2).storage()?),
        workspace: WorkspaceId::new(row.get::<_, String>(3).storage()?),
        scopes: scopes_from_sql(&row.get::<_, String>(4).storage()?)?,
        state,
        created_ms: from_sql(row.get(5).storage()?)?,
        expires_ms,
    };
    Ok((record, row.get(8).storage()?))
}

/// The record the presented text proves possession of, in any state. Unknown
/// or non-matching text is `Unauthenticated`.
fn verify(
    connection: &Connection,
    presented: &str,
    now_ms: u64,
) -> Result<CredentialRecord, StoreError> {
    let (prefix, secret_part) = parse(presented).ok_or(StoreError::Unauthenticated)?;
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM credentials WHERE prefix = ?1"
        ))
        .storage()?;
    let mut rows = statement.query([prefix]).storage()?;
    let row = rows.next().storage()?.ok_or(StoreError::Unauthenticated)?;
    let (record, digest) = record_from_row(row, now_ms)?;
    if !secret::verifies(&digest, prefix, secret_part) {
        return Err(StoreError::Unauthenticated);
    }
    Ok(record)
}

impl CredentialStore {
    /// Issues a scoped key for a service principal, bound to `workspace`, that
    /// stays valid until revoked.
    pub fn issue_key(
        &self,
        principal: &PrincipalId,
        workspace: &WorkspaceId,
        scopes: impl IntoIterator<Item = Scope>,
    ) -> Result<IssuedCredential, StoreError> {
        let holder = Holder::new(CredentialKind::ScopedKey, principal, workspace, scopes);
        self.write(|transaction, now_ms| holder.issue(transaction, now_ms, None))
    }

    /// Issues a login for a user after its approval, expiring after `lifetime`.
    pub fn issue_login(
        &self,
        principal: &PrincipalId,
        workspace: &WorkspaceId,
        scopes: impl IntoIterator<Item = Scope>,
        lifetime: Duration,
    ) -> Result<IssuedCredential, StoreError> {
        let holder = Holder::new(CredentialKind::LoginGrant, principal, workspace, scopes);
        self.write(|transaction, now_ms| {
            let expires_ms = expiry(now_ms, lifetime, "lifetime")?;
            holder.issue(transaction, now_ms, Some(expires_ms))
        })
    }

    /// Every scoped key bound to `workspace`, oldest first, in any state.
    pub fn list_keys(&self, workspace: &WorkspaceId) -> Result<Vec<CredentialRecord>, StoreError> {
        self.read(|transaction, now_ms| {
            let mut statement = transaction
                .prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM credentials WHERE workspace = ?1 AND kind = ?2
                     ORDER BY created_ms, prefix"
                ))
                .storage()?;
            let mut rows = statement
                .query(params![
                    workspace.as_str(),
                    CredentialKind::ScopedKey.name()
                ])
                .storage()?;
            let mut keys = Vec::new();
            while let Some(row) = rows.next().storage()? {
                keys.push(record_from_row(row, now_ms)?.0);
            }
            Ok(keys)
        })
    }

    /// Revokes the key with display `prefix` in `workspace`. Returns whether
    /// this call revoked it; a key outside the workspace is `NotFound`.
    pub fn revoke_key(&self, workspace: &WorkspaceId, prefix: &str) -> Result<bool, StoreError> {
        self.write(|transaction, now_ms| {
            let kind = CredentialKind::ScopedKey.name();
            let revoked = transaction
                .execute(
                    "UPDATE credentials SET revoked_ms = ?1
                     WHERE prefix = ?2 AND workspace = ?3 AND kind = ?4 AND revoked_ms IS NULL",
                    params![to_sql(now_ms)?, prefix, workspace.as_str(), kind],
                )
                .storage()?;
            if revoked == 1 {
                return Ok(true);
            }
            transaction
                .query_row(
                    "SELECT 1 FROM credentials WHERE prefix = ?1 AND workspace = ?2 AND kind = ?3",
                    params![prefix, workspace.as_str(), kind],
                    |_| Ok(false),
                )
                .optional()
                .storage()?
                .ok_or(StoreError::NotFound { what: "key" })
        })
    }

    /// Resolves presented key or login text to its principal. Malformed,
    /// unknown, revoked and expired text is `Unauthenticated` alike.
    pub fn authenticate(&self, presented: &str) -> Result<Principal, StoreError> {
        let record = self.read(|transaction, now_ms| verify(transaction, presented, now_ms))?;
        if record.state != CredentialState::Active {
            return Err(StoreError::Unauthenticated);
        }
        let credential = Credential::new(record.kind, record.workspace).with_scopes(record.scopes);
        Ok(Principal::new(record.principal, credential))
    }

    /// Revokes the caller's own login, the one `presented` proves. Returns
    /// `false` when it was already revoked or expired; other logins and keys
    /// are untouched. Key text is `Unauthenticated` here.
    pub fn logout(&self, presented: &str) -> Result<bool, StoreError> {
        self.write(|transaction, now_ms| {
            let record = verify(transaction, presented, now_ms)?;
            if record.kind != CredentialKind::LoginGrant {
                return Err(StoreError::Unauthenticated);
            }
            if record.state != CredentialState::Active {
                return Ok(false);
            }
            transaction
                .execute(
                    "UPDATE credentials SET revoked_ms = ?1 WHERE prefix = ?2",
                    params![to_sql(now_ms)?, record.prefix],
                )
                .storage()?;
            Ok(true)
        })
    }

    /// Revokes every active login `principal` holds in `workspace`, as member
    /// removal requires. Returns how many were revoked.
    pub fn revoke_logins(
        &self,
        principal: &PrincipalId,
        workspace: &WorkspaceId,
    ) -> Result<usize, StoreError> {
        self.write(|transaction, now_ms| {
            transaction
                .execute(
                    "UPDATE credentials SET revoked_ms = ?1
                     WHERE workspace = ?2 AND kind = ?3 AND principal = ?4
                       AND revoked_ms IS NULL AND (expires_ms IS NULL OR expires_ms > ?1)",
                    params![
                        to_sql(now_ms)?,
                        workspace.as_str(),
                        CredentialKind::LoginGrant.name(),
                        principal.as_str()
                    ],
                )
                .storage()
        })
    }
}

/// What a new credential is issued to.
struct Holder<'a> {
    kind: CredentialKind,
    principal: &'a PrincipalId,
    workspace: &'a WorkspaceId,
    scopes: BTreeSet<Scope>,
}

impl<'a> Holder<'a> {
    fn new(
        kind: CredentialKind,
        principal: &'a PrincipalId,
        workspace: &'a WorkspaceId,
        scopes: impl IntoIterator<Item = Scope>,
    ) -> Self {
        let scopes = scopes.into_iter().collect();
        Self {
            kind,
            principal,
            workspace,
            scopes,
        }
    }

    fn issue(
        self,
        connection: &Connection,
        now_ms: u64,
        expires_ms: Option<i64>,
    ) -> Result<IssuedCredential, StoreError> {
        required(self.principal.as_str(), "principal")?;
        required(self.workspace.as_str(), "workspace")?;
        let prefix = format!(
            "{}_{}",
            tag(self.kind),
            secret::random_symbols(PREFIX_SYMBOLS)?
        );
        let secret_part = secret::random_symbols(SECRET_SYMBOLS)?;
        connection
            .execute(
                "INSERT INTO credentials (prefix, kind, principal, workspace, scopes,
                                          secret_digest, created_ms, expires_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    prefix,
                    self.kind.name(),
                    self.principal.as_str(),
                    self.workspace.as_str(),
                    scopes_to_sql(&self.scopes),
                    secret::digest(&prefix, &secret_part).as_slice(),
                    to_sql(now_ms)?,
                    expires_ms,
                ],
            )
            .storage()?;
        let secret = SecretText::new(format!("{prefix}_{secret_part}"));
        let record = CredentialRecord {
            prefix,
            kind: self.kind,
            principal: self.principal.clone(),
            workspace: self.workspace.clone(),
            scopes: self.scopes,
            state: CredentialState::Active,
            created_ms: now_ms,
            expires_ms: expires_ms.map(from_sql).transpose()?,
        };
        Ok(IssuedCredential { record, secret })
    }
}
