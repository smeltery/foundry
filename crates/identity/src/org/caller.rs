//! The caller of an organization operation. It names the stored credential by
//! its public display prefix, and every operation reads that credential again
//! inside its own transaction, so a revocation, logout or expiry that commits
//! after the caller authenticated still refuses the operation.
use rusqlite::{OptionalExtension, Transaction, params};

use super::error::{OrgError, Storage};
use crate::authz::{Credential, CredentialKind, CredentialState, Principal, Scope, WorkspaceId};
use crate::store::database::to_sql;

/// A principal and the display prefix of the credential it authenticated
/// with, such as `CredentialRecord::prefix`. Only scoped keys and logins are
/// stored credentials; any other caller is refused as unauthenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    principal: Principal,
    prefix: String,
}

impl Caller {
    pub fn new(principal: Principal, prefix: impl Into<String>) -> Self {
        Self {
            principal,
            prefix: prefix.into(),
        }
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

/// The caller's principal rebuilt from the committed credential row: its
/// current state and scopes. A missing row, or one held by another principal,
/// workspace or kind, yields a revoked credential, which the decision core
/// refuses as unauthenticated.
pub(super) fn refresh(
    transaction: &Transaction<'_>,
    caller: &Caller,
    now_ms: u64,
) -> Result<Principal, OrgError> {
    let presented = &caller.principal;
    let revoked = || {
        Principal::new(
            presented.id.clone(),
            presented
                .credential
                .clone()
                .with_state(CredentialState::Revoked),
        )
    };
    let row = transaction
        .query_row(
            "SELECT kind, principal, workspace, scopes, expires_ms, revoked_ms
             FROM credentials WHERE prefix = ?1",
            params![caller.prefix],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .optional()
        .storage()?;
    let Some((kind, principal, workspace, scopes, expires_ms, revoked_ms)) = row else {
        return Ok(revoked());
    };
    let kind = CredentialKind::from_name(&kind)
        .ok_or(OrgError::unavailable("malformed stored credential kind"))?;
    if kind != presented.credential.kind
        || principal != presented.id.as_str()
        || workspace != presented.credential.workspace.as_str()
    {
        return Ok(revoked());
    }
    let now = to_sql(now_ms)?;
    let state = if revoked_ms.is_some() {
        CredentialState::Revoked
    } else if expires_ms.is_some_and(|expires| expires <= now) {
        CredentialState::Expired
    } else {
        CredentialState::Active
    };
    let scopes = scopes
        .split_whitespace()
        .map(|name| Scope::from_name(name).ok_or(OrgError::unavailable("malformed stored scope")))
        .collect::<Result<Vec<_>, _>>()?;
    let credential = Credential::new(kind, WorkspaceId::new(workspace))
        .with_scopes(scopes)
        .with_state(state);
    Ok(Principal::new(presented.id.clone(), credential))
}
