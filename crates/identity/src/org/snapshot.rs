//! Builds the committed state the decision core reads: the caller's bound
//! workspace and, for a retirement, the single-use workspace it names. Nothing
//! is cached; every request loads its own snapshot inside its transaction.
use rusqlite::{OptionalExtension, Transaction, params};

use super::error::{OrgError, Storage};
use crate::authz::{
    OperationRequest, Organization, Principal, PrincipalId, Role, Workspace, WorkspaceId,
    WorkspaceState,
};
use crate::store::database::to_sql;

pub(super) fn load(
    transaction: &Transaction<'_>,
    caller: &Principal,
    request: &OperationRequest,
    now_ms: u64,
) -> Result<Organization, OrgError> {
    let mut organization = Organization::new();
    let targets = [
        Some(&caller.credential.workspace),
        request.single_use.as_ref(),
    ];
    for id in targets.into_iter().flatten() {
        if let Some(workspace) = workspace(transaction, id, now_ms)? {
            organization = organization.with_workspace(workspace);
        }
    }
    Ok(organization)
}

/// A single-use workspace past its expiry is retired for authorization even
/// before its retirement is recorded, so its token stops at expiry.
fn workspace(
    transaction: &Transaction<'_>,
    id: &WorkspaceId,
    now_ms: u64,
) -> Result<Option<Workspace>, OrgError> {
    let row = transaction
        .query_row(
            "SELECT state, parent, expires_ms IS NOT NULL AND expires_ms <= ?2
             FROM org_workspaces WHERE id = ?1",
            params![id.as_str(), to_sql(now_ms)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()
        .storage()?;
    let Some((state, parent, expired)) = row else {
        return Ok(None);
    };
    let state = match parse_state(&state)? {
        WorkspaceState::Active if expired => WorkspaceState::Retired,
        state => state,
    };
    let mut workspace = Workspace::new(id.clone()).with_state(state);
    if let Some(parent) = parent {
        workspace = workspace.single_use_of(WorkspaceId::new(parent));
    }
    let mut statement = transaction
        .prepare("SELECT principal, role FROM org_members WHERE workspace = ?1")
        .storage()?;
    let members = statement
        .query_map([id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .storage()?;
    for member in members {
        let (principal, role) = member.storage()?;
        workspace = workspace.with_member(PrincipalId::new(principal), parse_role(&role)?);
    }
    Ok(Some(workspace))
}

pub(super) fn state_name(state: WorkspaceState) -> &'static str {
    match state {
        WorkspaceState::Active => "active",
        WorkspaceState::Erasing => "erasing",
        WorkspaceState::Deleted => "deleted",
        WorkspaceState::Retired => "retired",
    }
}

fn parse_state(name: &str) -> Result<WorkspaceState, OrgError> {
    [
        WorkspaceState::Active,
        WorkspaceState::Erasing,
        WorkspaceState::Deleted,
        WorkspaceState::Retired,
    ]
    .into_iter()
    .find(|state| state_name(*state) == name)
    .ok_or(OrgError::unavailable("malformed stored workspace state"))
}

pub(super) fn parse_role(name: &str) -> Result<Role, OrgError> {
    Role::from_name(name).ok_or(OrgError::unavailable("malformed stored role"))
}
