//! Pairing codes: short, single-use and short-lived, presented as
//! `<id>-<secret>` where `<id>` is the public prefix. A wrong secret for a
//! known prefix, presented by the issuing user, counts against that pairing's
//! attempt limit; once the limit is reached the pairing never verifies again.
//! The first matching claim binds the pairing to one client; only that client
//! may repeat the claim. The secret's digest is erased once the pairing is
//! claimed, exhausted or expired, so a later copy of the database holds
//! nothing to search.
//!
//! Approval, failure reports and completion of the import handoff are held
//! under user decision C4 and are not stored here.
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::database::{expiry, from_sql, to_sql};
use super::error::{Storage, StoreError};
use super::secret::{self, SecretText};
use super::{CredentialStore, bearer, required};
use crate::authz::{CredentialKind, CredentialState, PrincipalId, WorkspaceId};

const PREFIX_SYMBOLS: usize = 8;
/// Eight symbols of five bits each: 40 random bits, typed by hand.
const SECRET_SYMBOLS: usize = 8;

/// The longest pairing lifetime. The specification calls the window
/// short-lived but gives no duration, so this provisional cap keeps it to a
/// few minutes.
pub const PAIRING_MAX_LIFETIME: Duration = Duration::from_secs(600);

/// The most wrong secrets a pairing accepts before it is exhausted. The
/// specification gives no count; this one is provisional.
pub const PAIRING_MAX_ATTEMPTS: u32 = 5;

/// Erases the digest of every pairing whose lifetime has passed. Claimed and
/// exhausted pairings lose theirs in the write that ends them; an expired one
/// keeps its digest until the next pairing write.
fn clear_lapsed_digests(connection: &Connection, now_ms: u64) -> Result<(), StoreError> {
    connection
        .execute(
            "UPDATE pairings SET secret_digest = NULL
             WHERE secret_digest IS NOT NULL AND expires_ms <= ?1",
            [to_sql(now_ms)?],
        )
        .storage()?;
    Ok(())
}

/// Provisional state names; `exhausted` is not in the specification's pairing
/// machine, which leaves attempt limits open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingState {
    Issued,
    Claimed,
    Expired,
    /// The attempt limit was reached before a successful claim.
    Exhausted,
}

impl PairingState {
    pub fn name(self) -> &'static str {
        match self {
            PairingState::Issued => "issued",
            PairingState::Claimed => "claimed",
            PairingState::Expired => "expired",
            PairingState::Exhausted => "exhausted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub prefix: String,
    /// The user who issued the code; only that user's login may claim it.
    pub principal: PrincipalId,
    pub workspace: WorkspaceId,
    pub state: PairingState,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub max_attempts: u32,
    pub failed_attempts: u32,
    /// The client that claimed the pairing.
    pub client: Option<String>,
}

/// A newly issued pairing. `code` is the only copy of the presentable text.
#[derive(Debug, Clone)]
pub struct IssuedPairing {
    pub record: PairingRecord,
    pub code: SecretText,
}

fn parse(code: &str) -> Option<(&str, &str)> {
    let (prefix, secret_part) = code.split_once('-')?;
    let well_formed = secret::is_symbols(prefix, PREFIX_SYMBOLS)
        && secret::is_symbols(secret_part, SECRET_SYMBOLS);
    well_formed.then_some((prefix, secret_part))
}

/// A pairing row and its digest, `None` once erased.
type Stored = (PairingRecord, Option<Vec<u8>>);

fn load(connection: &Connection, prefix: &str, now_ms: u64) -> Result<Option<Stored>, StoreError> {
    let row = connection
        .query_row(
            "SELECT principal, workspace, created_ms, expires_ms, max_attempts, failed_attempts,
                    client, secret_digest
             FROM pairings WHERE prefix = ?1",
            [prefix],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                ))
            },
        )
        .optional()
        .storage()?;
    let Some((principal, workspace, created, expires, max_attempts, failed, client, digest)) = row
    else {
        return Ok(None);
    };
    let expires_ms = from_sql(expires)?;
    let state = if client.is_some() {
        PairingState::Claimed
    } else if failed >= max_attempts {
        PairingState::Exhausted
    } else if now_ms >= expires_ms {
        PairingState::Expired
    } else {
        PairingState::Issued
    };
    let record = PairingRecord {
        prefix: prefix.to_owned(),
        principal: PrincipalId::new(principal),
        workspace: WorkspaceId::new(workspace),
        state,
        created_ms: from_sql(created)?,
        expires_ms,
        max_attempts,
        failed_attempts: failed,
        client,
    };
    Ok(Some((record, digest)))
}

impl CredentialStore {
    /// Issues a pairing code for `principal` in `workspace`, valid for
    /// `lifetime` (at most [`PAIRING_MAX_LIFETIME`]) and for at most
    /// `max_attempts` wrong secrets (1 to [`PAIRING_MAX_ATTEMPTS`]).
    pub fn issue_pairing(
        &self,
        principal: &PrincipalId,
        workspace: &WorkspaceId,
        lifetime: Duration,
        max_attempts: u32,
    ) -> Result<IssuedPairing, StoreError> {
        required(principal.as_str(), "principal")?;
        required(workspace.as_str(), "workspace")?;
        if !(1..=PAIRING_MAX_ATTEMPTS).contains(&max_attempts) {
            return Err(StoreError::InvalidRequest {
                field: "max_attempts",
            });
        }
        if lifetime > PAIRING_MAX_LIFETIME {
            return Err(StoreError::InvalidRequest { field: "lifetime" });
        }
        self.write(|transaction, now_ms| {
            clear_lapsed_digests(transaction, now_ms)?;
            let expires = expiry(now_ms, lifetime, "lifetime")?;
            let prefix = secret::random_symbols(PREFIX_SYMBOLS)?;
            let secret_part = secret::random_symbols(SECRET_SYMBOLS)?;
            transaction
                .execute(
                    "INSERT INTO pairings (prefix, principal, workspace, secret_digest, created_ms,
                                           expires_ms, max_attempts, failed_attempts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                    params![
                        prefix,
                        principal.as_str(),
                        workspace.as_str(),
                        secret::digest(&prefix, &secret_part).as_slice(),
                        to_sql(now_ms)?,
                        expires,
                        max_attempts,
                    ],
                )
                .storage()?;
            let code = SecretText::new(format!("{prefix}-{secret_part}"));
            let record = PairingRecord {
                prefix,
                principal: principal.clone(),
                workspace: workspace.clone(),
                state: PairingState::Issued,
                created_ms: now_ms,
                expires_ms: from_sql(expires)?,
                max_attempts,
                failed_attempts: 0,
                client: None,
            };
            Ok(IssuedPairing { record, code })
        })
    }

    /// Claims a pairing for `client`. `presented_login` is verified in the
    /// same transaction and must be an active login of the issuing user in the
    /// issuing workspace; any other caller is refused before the secret is
    /// checked, so it cannot use up the issuer's attempts. Unknown, revoked or
    /// expired login text is `Unauthenticated`; every other refusal is
    /// `PairingInvalid`. A wrong secret uses up one attempt, committed even
    /// though the claim fails. A claim erases the digest, so the same login
    /// and client repeating it get the pairing back on its prefix alone.
    pub fn claim_pairing(
        &self,
        presented_login: &str,
        client: &str,
        code: &str,
    ) -> Result<PairingRecord, StoreError> {
        required(client, "client")?;
        let (prefix, secret_part) = parse(code).ok_or(StoreError::PairingInvalid)?;
        self.write(|transaction, now_ms| {
            let login = bearer::verify(transaction, presented_login, now_ms)?;
            if login.state != CredentialState::Active {
                return Err(StoreError::Unauthenticated);
            }
            clear_lapsed_digests(transaction, now_ms)?;
            let Some((mut record, digest)) = load(transaction, prefix, now_ms)? else {
                return Ok(Err(StoreError::PairingInvalid));
            };
            let holder = login.kind == CredentialKind::LoginGrant
                && login.principal == record.principal
                && login.workspace == record.workspace;
            if !holder {
                return Ok(Err(StoreError::PairingInvalid));
            }
            match record.state {
                PairingState::Claimed if record.client.as_deref() == Some(client) => Ok(Ok(record)),
                PairingState::Issued => {
                    let verified =
                        digest.is_some_and(|digest| secret::verifies(&digest, prefix, secret_part));
                    if !verified {
                        // The attempt that reaches the limit also erases the digest.
                        transaction
                            .execute(
                                "UPDATE pairings SET failed_attempts = failed_attempts + 1,
                                     secret_digest = CASE
                                         WHEN failed_attempts + 1 >= max_attempts THEN NULL
                                         ELSE secret_digest END
                                 WHERE prefix = ?1",
                                [prefix],
                            )
                            .storage()?;
                        return Ok(Err(StoreError::PairingInvalid));
                    }
                    transaction
                        .execute(
                            "UPDATE pairings SET client = ?1, claimed_ms = ?2, secret_digest = NULL
                             WHERE prefix = ?3",
                            params![client, to_sql(now_ms)?, prefix],
                        )
                        .storage()?;
                    record.state = PairingState::Claimed;
                    record.client = Some(client.to_owned());
                    Ok(Ok(record))
                }
                _ => Ok(Err(StoreError::PairingInvalid)),
            }
        })?
    }

    /// The pairing with public `prefix` in `workspace`; one in another
    /// workspace is `NotFound`.
    pub fn pairing(
        &self,
        workspace: &WorkspaceId,
        prefix: &str,
    ) -> Result<PairingRecord, StoreError> {
        self.read(|transaction, now_ms| {
            load(transaction, prefix, now_ms)?
                .map(|(record, _)| record)
                .filter(|record| &record.workspace == workspace)
                .ok_or(StoreError::NotFound { what: "pairing" })
        })
    }
}
