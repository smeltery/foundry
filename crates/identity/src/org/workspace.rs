//! Workspace create, list, get, rename and delete.
use rusqlite::{OptionalExtension, params};

use super::error::{OrgCause, OrgError, Storage};
use super::snapshot::{parse_role, state_name};
use super::{Authorized, Caller, Change, OrgStore, new_id, nonblank};
use crate::authz::{OperationRequest, Role, WorkspaceId, WorkspaceState};
use crate::store::database::to_sql;

/// A workspace as the caller sees it, with the caller's role in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub name: String,
    /// Derived from the name; never supplied by the caller.
    pub handle: String,
    pub role: Option<Role>,
    /// The paying parent when this is a single-use workspace.
    pub parent: Option<WorkspaceId>,
}

/// Lowercase letters and digits, with every other run of characters turned
/// into one `-` and none at either end.
pub(super) fn derive_handle(name: &str) -> String {
    let mut handle = String::with_capacity(name.len());
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            handle.push(character);
        } else if !handle.is_empty() && !handle.ends_with('-') {
            handle.push('-');
        }
    }
    handle.trim_end_matches('-').to_owned()
}

impl OrgStore {
    /// Creates a workspace owned by the caller. Existing credentials stay bound
    /// to their own workspace; a login grant is refused by the decision core.
    pub fn create_workspace(
        &self,
        caller: &Caller,
        name: &str,
    ) -> Result<WorkspaceRecord, OrgError> {
        self.write(
            caller,
            OperationRequest::new("workspace.create"),
            |authorized| {
                let name = nonblank(name, "name")?;
                let id = WorkspaceId::new(new_id("ws")?);
                let handle = derive_handle(name);
                let now = to_sql(authorized.now_ms)?;
                authorized
                    .transaction
                    .execute(
                        "INSERT INTO org_workspaces (id, name, handle, state, created_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            id.as_str(),
                            name,
                            handle,
                            state_name(WorkspaceState::Active),
                            now
                        ],
                    )
                    .storage()?;
                authorized
                    .transaction
                    .execute(
                        "INSERT INTO org_members (workspace, principal, role, joined_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                        params![
                            id.as_str(),
                            authorized.caller.id.as_str(),
                            Role::Owner.name(),
                            now
                        ],
                    )
                    .storage()?;
                authorized.record(
                    &id,
                    Change {
                        action: "workspace.create",
                        target_kind: "workspace",
                        target: id.as_str(),
                        label: Some(name),
                        details: format!("handle={handle}"),
                    },
                )?;
                Ok(WorkspaceRecord {
                    id: id.clone(),
                    name: name.to_owned(),
                    handle,
                    role: Some(Role::Owner),
                    parent: None,
                })
            },
        )
    }

    /// Every workspace the credential can act on: exactly its bound one.
    pub fn list_workspaces(&self, caller: &Caller) -> Result<Vec<WorkspaceRecord>, OrgError> {
        self.read(
            caller,
            OperationRequest::new("workspace.list"),
            |authorized| {
                Ok(vec![
                    authorized.workspace(&authorized.caller.credential.workspace)?,
                ])
            },
        )
    }

    pub fn get_workspace(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceRecord, OrgError> {
        let request = OperationRequest::new("workspace.get").in_workspace(workspace.clone());
        self.read(caller, request, |authorized| {
            authorized.workspace(workspace)
        })
    }

    /// Renames and derives the handle again. The same name changes nothing and
    /// records no entry.
    pub fn rename_workspace(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        name: &str,
    ) -> Result<WorkspaceRecord, OrgError> {
        let request = OperationRequest::new("workspace.rename").in_workspace(workspace.clone());
        self.write(caller, request, |authorized| {
            let name = nonblank(name, "name")?;
            let current = authorized.workspace(workspace)?;
            if current.name == name {
                return Ok(current);
            }
            let handle = derive_handle(name);
            authorized
                .transaction
                .execute(
                    "UPDATE org_workspaces SET name = ?2, handle = ?3 WHERE id = ?1",
                    params![workspace.as_str(), name, handle],
                )
                .storage()?;
            authorized.record(
                workspace,
                Change {
                    action: "workspace.rename",
                    target_kind: "workspace",
                    target: workspace.as_str(),
                    label: Some(name),
                    details: format!("from={} handle={handle}", current.name),
                },
            )?;
            Ok(WorkspaceRecord {
                name: name.to_owned(),
                handle,
                ..current
            })
        })
    }

    /// Deletes the workspace when `confirm_name` equals its current name
    /// exactly. In the same commit it revokes every credential bound to it
    /// and retires each of its live single-use workspaces, with their own
    /// audit entries, since nothing would be left to pay for or retire them.
    pub fn delete_workspace(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        confirm_name: &str,
    ) -> Result<(), OrgError> {
        let request = OperationRequest::new("workspace.delete").in_workspace(workspace.clone());
        self.write(caller, request, |authorized| {
            let current = authorized.workspace(workspace)?;
            if current.name != confirm_name {
                return Err(OrgError::InvalidRequest {
                    field: "confirm_name",
                    cause: Some(OrgCause::NameMismatch),
                });
            }
            for child in authorized.live_single_use(workspace)? {
                authorized.retire_single_use(workspace, &child)?;
            }
            let now = to_sql(authorized.now_ms)?;
            authorized
                .transaction
                .execute(
                    "UPDATE org_workspaces SET state = ?2 WHERE id = ?1",
                    params![workspace.as_str(), state_name(WorkspaceState::Deleted)],
                )
                .storage()?;
            authorized
                .transaction
                .execute(
                    "UPDATE credentials SET revoked_ms = ?2
                     WHERE workspace = ?1 AND revoked_ms IS NULL",
                    params![workspace.as_str(), now],
                )
                .storage()?;
            authorized.record(
                workspace,
                Change {
                    action: "workspace.delete",
                    target_kind: "workspace",
                    target: workspace.as_str(),
                    label: Some(&current.name),
                    details: String::new(),
                },
            )
        })
    }
}

impl Authorized<'_> {
    /// The stored workspace with the caller's role in it.
    pub(super) fn workspace(&self, id: &WorkspaceId) -> Result<WorkspaceRecord, OrgError> {
        let row = self
            .transaction
            .query_row(
                "SELECT w.name, w.handle, w.parent, m.role
                 FROM org_workspaces w
                 LEFT JOIN org_members m ON m.workspace = w.id AND m.principal = ?2
                 WHERE w.id = ?1",
                params![id.as_str(), self.caller.id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .storage()?;
        let (name, handle, parent, role) = row.ok_or(OrgError::NotFound {
            what: "workspace",
            cause: None,
        })?;
        Ok(WorkspaceRecord {
            id: id.clone(),
            name,
            handle,
            role: role.as_deref().map(parse_role).transpose()?,
            parent: parent.map(WorkspaceId::new),
        })
    }
}
