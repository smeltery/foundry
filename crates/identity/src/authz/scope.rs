//! Scope and role vocabulary. Wire names are provisional pending the naming
//! decisions; no scope implies another.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    DeploymentRead,
    WorkspaceRead,
    WorkspaceWrite,
    AuditRead,
    NetworkPolicyRead,
    NetworkPolicyWrite,
    SessionsRead,
    SessionsRun,
    MachinesRead,
    MachinesWrite,
    MachinesUse,
}

impl Scope {
    pub const ALL: [Scope; 11] = [
        Scope::DeploymentRead,
        Scope::WorkspaceRead,
        Scope::WorkspaceWrite,
        Scope::AuditRead,
        Scope::NetworkPolicyRead,
        Scope::NetworkPolicyWrite,
        Scope::SessionsRead,
        Scope::SessionsRun,
        Scope::MachinesRead,
        Scope::MachinesWrite,
        Scope::MachinesUse,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scope::DeploymentRead => "deployment.read",
            Scope::WorkspaceRead => "workspace.read",
            Scope::WorkspaceWrite => "workspace.write",
            Scope::AuditRead => "audit.read",
            Scope::NetworkPolicyRead => "network_policy.read",
            Scope::NetworkPolicyWrite => "network_policy.write",
            Scope::SessionsRead => "sessions.read",
            Scope::SessionsRun => "sessions.run",
            Scope::MachinesRead => "machines.read",
            Scope::MachinesWrite => "machines.write",
            Scope::MachinesUse => "machines.use",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.name() == name)
    }
}

/// A workspace role. It belongs to one membership in one workspace; a product
/// tier is never a role and never enters a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Owner, Role::Admin, Role::Member, Role::Viewer];

    pub fn name(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Member => "member",
            Role::Viewer => "viewer",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.name() == name)
    }
}
