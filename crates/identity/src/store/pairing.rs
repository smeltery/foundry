//! Pairing codes: short, single-use and short-lived, presented as
//! `<id>-<secret>` where `<id>` is the public prefix. A wrong secret for a
//! known prefix counts against that pairing's attempt limit, and once the limit
//! is reached the pairing never verifies again. The first matching claim binds
//! the pairing to one client; only that client may repeat the claim.
//!
//! Approval, failure reports and completion of the import handoff are held
//! under user decision C4 and are not stored here.
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::database::{expiry, from_sql, to_sql};
use super::error::{Storage, StoreError};
use super::secret::{self, SecretText};
use super::{CredentialStore, required};
use crate::authz::{CredentialKind, CredentialState, Principal, PrincipalId, WorkspaceId};

const PREFIX_SYMBOLS: usize = 8;
/// Eight symbols of five bits each: 40 random bits, typed by hand.
const SECRET_SYMBOLS: usize = 8;

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

fn load(
    connection: &Connection,
    prefix: &str,
    now_ms: u64,
) -> Result<Option<(PairingRecord, Vec<u8>)>, StoreError> {
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
                    row.get::<_, Vec<u8>>(7)?,
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
    /// `lifetime` and for at most `max_attempts` wrong secrets.
    pub fn issue_pairing(
        &self,
        principal: &PrincipalId,
        workspace: &WorkspaceId,
        lifetime: Duration,
        max_attempts: u32,
    ) -> Result<IssuedPairing, StoreError> {
        required(principal.as_str(), "principal")?;
        required(workspace.as_str(), "workspace")?;
        if max_attempts == 0 {
            return Err(StoreError::InvalidRequest {
                field: "max_attempts",
            });
        }
        self.write(|transaction, now_ms| {
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

    /// Claims a pairing for `client` with the caller's active login. The
    /// claimant must be the issuing user in the issuing workspace. Every
    /// refusal is `PairingInvalid`; a wrong secret also uses up one attempt,
    /// which is committed even though the claim fails.
    pub fn claim_pairing(
        &self,
        claimant: &Principal,
        client: &str,
        code: &str,
    ) -> Result<PairingRecord, StoreError> {
        required(client, "client")?;
        let (prefix, secret_part) = parse(code).ok_or(StoreError::PairingInvalid)?;
        let login = claimant.credential.kind == CredentialKind::LoginGrant
            && claimant.credential.state == CredentialState::Active;
        self.write(|transaction, now_ms| {
            let Some((mut record, digest)) = load(transaction, prefix, now_ms)? else {
                return Ok(Err(StoreError::PairingInvalid));
            };
            if !secret::verifies(&digest, prefix, secret_part) {
                if record.state == PairingState::Issued {
                    transaction
                        .execute(
                            "UPDATE pairings SET failed_attempts = failed_attempts + 1
                             WHERE prefix = ?1",
                            [prefix],
                        )
                        .storage()?;
                }
                return Ok(Err(StoreError::PairingInvalid));
            }
            let holder = login
                && claimant.id == record.principal
                && claimant.credential.workspace == record.workspace;
            match record.state {
                PairingState::Issued if holder => {
                    transaction
                        .execute(
                            "UPDATE pairings SET client = ?1, claimed_ms = ?2 WHERE prefix = ?3",
                            params![client, to_sql(now_ms)?, prefix],
                        )
                        .storage()?;
                    record.state = PairingState::Claimed;
                    record.client = Some(client.to_owned());
                    Ok(Ok(record))
                }
                PairingState::Claimed if holder && record.client.as_deref() == Some(client) => {
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
