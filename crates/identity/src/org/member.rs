//! Membership: list, invite, get, remove and role changes. The decision core
//! enforces that only an owner touches owner memberships; the rule that a
//! workspace always keeps one owner is checked here against the state read
//! under the write lock.
use rusqlite::{OptionalExtension, params};

use super::error::{OrgCause, OrgError, Storage};
use super::snapshot::parse_role;
use super::{Authorized, Caller, Change, OrgStore, nonblank};
use crate::authz::{Access, CredentialKind, OperationRequest, PrincipalId, Role, WorkspaceId};
use crate::store::database::{from_sql, to_sql};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    pub user: PrincipalId,
    /// Absent until the identity authority links an address (decision C1).
    pub email: Option<String>,
    pub role: Role,
    pub joined_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub email: String,
    pub role: Role,
    pub created_ms: u64,
}

/// Members, and pending invitations oldest first. Callers with partial access
/// receive an empty invitation list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberList {
    pub members: Vec<MemberRecord>,
    pub invitations: Vec<Invitation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteOutcome {
    pub role: Role,
    pub invited: usize,
    /// Notices actually delivered. Delivery is held under decision C1, so a
    /// local authority records invitations and delivers none.
    pub delivered: usize,
}

/// One address: no whitespace, and exactly one `@` with text on both sides.
fn address(text: &str) -> Result<&str, OrgError> {
    let text = nonblank(text, "emails")?;
    let well_formed = !text.contains(char::is_whitespace)
        && text.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
    well_formed
        .then_some(text)
        .ok_or(OrgError::invalid("emails"))
}

impl OrgStore {
    pub fn list_members(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
    ) -> Result<MemberList, OrgError> {
        let request = OperationRequest::new("member.list").in_workspace(workspace.clone());
        self.read(caller, request, |authorized| {
            let mut statement = authorized
                .transaction
                .prepare(
                    "SELECT principal, email, role, joined_ms FROM org_members
                     WHERE workspace = ?1 ORDER BY joined_ms, principal",
                )
                .storage()?;
            let rows = statement
                .query_map([workspace.as_str()], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get::<_, String>(2)?,
                        row.get(3)?,
                    ))
                })
                .storage()?;
            let mut members = Vec::new();
            for row in rows {
                let (user, email, role, joined_ms): (String, _, _, i64) = row.storage()?;
                members.push(MemberRecord {
                    user: PrincipalId::new(user),
                    email,
                    role: parse_role(&role)?,
                    joined_ms: from_sql(joined_ms)?,
                });
            }
            let invitations = match authorized.access {
                Access::Full => authorized.invitations(workspace)?,
                Access::Partial => Vec::new(),
            };
            Ok(MemberList {
                members,
                invitations,
            })
        })
    }

    /// Records one invitation per address with `role`, `admin` by default.
    /// The owner role is granted only after joining.
    pub fn invite_members(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        emails: &[&str],
        role: Option<Role>,
    ) -> Result<InviteOutcome, OrgError> {
        let request = OperationRequest::new("member.invite").in_workspace(workspace.clone());
        self.write(caller, request, |authorized| {
            let role = role.unwrap_or(Role::Admin);
            if !matches!(role, Role::Admin | Role::Member) {
                return Err(OrgError::invalid("role"));
            }
            if emails.is_empty() {
                return Err(OrgError::invalid("emails"));
            }
            let addresses = emails
                .iter()
                .map(|email| address(email))
                .collect::<Result<Vec<_>, _>>()?;
            for email in &addresses {
                authorized
                    .transaction
                    .execute(
                        "INSERT INTO org_invitations (workspace, email, role, created_ms)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            workspace.as_str(),
                            email,
                            role.name(),
                            to_sql(authorized.now_ms)?
                        ],
                    )
                    .storage()?;
                authorized.record(
                    workspace,
                    Change {
                        action: "member.invite",
                        target_kind: "invitation",
                        target: email,
                        label: None,
                        details: format!("role={}", role.name()),
                    },
                )?;
            }
            Ok(InviteOutcome {
                role,
                invited: addresses.len(),
                delivered: 0,
            })
        })
    }

    pub fn get_member(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        user: &PrincipalId,
    ) -> Result<MemberRecord, OrgError> {
        let request = OperationRequest::new("member.get")
            .in_workspace(workspace.clone())
            .for_member(user.clone());
        self.read(caller, request, |authorized| {
            authorized
                .member(workspace, user)?
                .ok_or(OrgError::not_member())
        })
    }

    /// Ends the membership only, and revokes the user's login grants for this
    /// workspace in the same commit. The account and other memberships stay.
    pub fn remove_member(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        user: &PrincipalId,
    ) -> Result<(), OrgError> {
        let request = OperationRequest::new("member.remove")
            .in_workspace(workspace.clone())
            .for_member(user.clone());
        self.write(caller, request, |authorized| {
            let target = authorized
                .member(workspace, user)?
                .ok_or(OrgError::not_member())?;
            if target.role == Role::Owner {
                authorized.keep_an_owner(workspace)?;
            }
            // Recorded before the delete, so a self-removal keeps the actor's
            // email; the transaction commits both or neither.
            authorized.record(
                workspace,
                Change {
                    action: "member.remove",
                    target_kind: "member",
                    target: user.as_str(),
                    label: target.email.as_deref(),
                    details: format!("role={}", target.role.name()),
                },
            )?;
            let transaction = authorized.transaction;
            transaction
                .execute(
                    "DELETE FROM org_members WHERE workspace = ?1 AND principal = ?2",
                    params![workspace.as_str(), user.as_str()],
                )
                .storage()?;
            transaction
                .execute(
                    "UPDATE credentials SET revoked_ms = ?4
                     WHERE workspace = ?1 AND principal = ?2 AND kind = ?3
                       AND revoked_ms IS NULL",
                    params![
                        workspace.as_str(),
                        user.as_str(),
                        CredentialKind::LoginGrant.name(),
                        to_sql(authorized.now_ms)?,
                    ],
                )
                .storage()?;
            Ok(())
        })
    }

    /// Sets `owner`, `admin` or `member`. Repeating the current role changes
    /// nothing and records no entry. Product tiers and credential scopes are
    /// never touched.
    pub fn set_member_role(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        user: &PrincipalId,
        role: Role,
    ) -> Result<MemberRecord, OrgError> {
        let request = OperationRequest::new("member.set_role")
            .in_workspace(workspace.clone())
            .for_member(user.clone())
            .assigning(role);
        self.write(caller, request, |authorized| {
            if role == Role::Viewer {
                return Err(OrgError::invalid("role"));
            }
            let target = authorized
                .member(workspace, user)?
                .ok_or(OrgError::not_member())?;
            if target.role == role {
                return Ok(target);
            }
            if target.role == Role::Owner {
                authorized.keep_an_owner(workspace)?;
            }
            authorized
                .transaction
                .execute(
                    "UPDATE org_members SET role = ?3 WHERE workspace = ?1 AND principal = ?2",
                    params![workspace.as_str(), user.as_str(), role.name()],
                )
                .storage()?;
            authorized.record(
                workspace,
                Change {
                    action: "member.set_role",
                    target_kind: "member",
                    target: user.as_str(),
                    label: target.email.as_deref(),
                    details: format!("from={} to={}", target.role.name(), role.name()),
                },
            )?;
            Ok(MemberRecord { role, ..target })
        })
    }
}

impl Authorized<'_> {
    fn member(
        &self,
        workspace: &WorkspaceId,
        user: &PrincipalId,
    ) -> Result<Option<MemberRecord>, OrgError> {
        let row = self
            .transaction
            .query_row(
                "SELECT email, role, joined_ms FROM org_members
                 WHERE workspace = ?1 AND principal = ?2",
                params![workspace.as_str(), user.as_str()],
                |row| Ok((row.get(0)?, row.get::<_, String>(1)?, row.get(2)?)),
            )
            .optional()
            .storage()?;
        row.map(|(email, role, joined_ms)| {
            Ok(MemberRecord {
                user: user.clone(),
                email,
                role: parse_role(&role)?,
                joined_ms: from_sql(joined_ms)?,
            })
        })
        .transpose()
    }

    /// Refuses to remove or demote an owner when it is the only one.
    fn keep_an_owner(&self, workspace: &WorkspaceId) -> Result<(), OrgError> {
        let owners: i64 = self
            .transaction
            .query_row(
                "SELECT count(*) FROM org_members WHERE workspace = ?1 AND role = ?2",
                params![workspace.as_str(), Role::Owner.name()],
                |row| row.get(0),
            )
            .storage()?;
        if owners <= 1 {
            return Err(OrgError::Conflict {
                cause: OrgCause::LastOwner,
            });
        }
        Ok(())
    }

    fn invitations(&self, workspace: &WorkspaceId) -> Result<Vec<Invitation>, OrgError> {
        let mut statement = self
            .transaction
            .prepare(
                "SELECT email, role, created_ms FROM org_invitations
                 WHERE workspace = ?1 ORDER BY sequence",
            )
            .storage()?;
        let rows = statement
            .query_map([workspace.as_str()], |row| {
                Ok((row.get(0)?, row.get::<_, String>(1)?, row.get(2)?))
            })
            .storage()?;
        let mut invitations = Vec::new();
        for row in rows {
            let (email, role, created_ms): (String, String, i64) = row.storage()?;
            invitations.push(Invitation {
                email,
                role: parse_role(&role)?,
                created_ms: from_sql(created_ms)?,
            });
        }
        Ok(invitations)
    }
}
