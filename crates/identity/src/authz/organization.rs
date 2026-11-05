//! The committed identity state a decision reads: workspaces, their
//! memberships and their machines. Callers pass a fresh snapshot per request;
//! decisions are never cached, so every committed revocation applies to the
//! next decision.

use std::collections::{BTreeMap, BTreeSet};

use super::ids::{MachineId, PrincipalId, WorkspaceId};
use super::scope::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Usable; a live single-use workspace is also active.
    Active,
    /// Deletion is erasing in steps. Bound credentials stay valid so the
    /// owner can repeat the deletion.
    Erasing,
    /// Deleted; every bound credential is revoked.
    Deleted,
    /// A retired single-use workspace; its token and machines are revoked.
    Retired,
}

impl WorkspaceState {
    /// Whether credentials bound to a workspace in this state still authenticate.
    pub fn keeps_credentials(self) -> bool {
        matches!(self, WorkspaceState::Active | WorkspaceState::Erasing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub state: WorkspaceState,
    members: BTreeMap<PrincipalId, Role>,
    machines: BTreeSet<MachineId>,
}

impl Workspace {
    /// An active workspace with no members or machines.
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            state: WorkspaceState::Active,
            members: BTreeMap::new(),
            machines: BTreeSet::new(),
        }
    }

    pub fn with_state(mut self, state: WorkspaceState) -> Self {
        self.state = state;
        self
    }

    /// Adds a membership, replacing any earlier role of that user.
    pub fn with_member(mut self, user: PrincipalId, role: Role) -> Self {
        self.members.insert(user, role);
        self
    }

    /// Registers a machine; unregistered machines are revoked.
    pub fn with_machine(mut self, machine: MachineId) -> Self {
        self.machines.insert(machine);
        self
    }

    pub fn role_of(&self, user: &PrincipalId) -> Option<Role> {
        self.members.get(user).copied()
    }

    pub fn has_machine(&self, machine: &MachineId) -> bool {
        self.machines.contains(machine)
    }
}

/// Every workspace the authority knows about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Organization {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
}

impl Organization {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a workspace, replacing any earlier one with the same ID.
    pub fn with_workspace(mut self, workspace: Workspace) -> Self {
        self.workspaces.insert(workspace.id.clone(), workspace);
        self
    }

    pub fn workspace(&self, id: &WorkspaceId) -> Option<&Workspace> {
        self.workspaces.get(id)
    }
}
