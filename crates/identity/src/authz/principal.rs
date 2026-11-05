//! Principals and the credential each one authenticated with.

use std::collections::BTreeSet;

use super::ids::{MachineId, PrincipalId, WorkspaceId};
use super::scope::Scope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    ScopedKey,
    LoginGrant,
    AdminGrant,
    MachineToken,
}

/// The fixed restricted set a machine token may carry, whatever it was issued with.
const MACHINE_TOKEN_SCOPES: [Scope; 6] = [
    Scope::SessionsRead,
    Scope::SessionsRun,
    Scope::MachinesRead,
    Scope::MachinesWrite,
    Scope::MachinesUse,
    Scope::DeploymentRead,
];

impl CredentialKind {
    pub const ALL: [CredentialKind; 4] = [
        CredentialKind::ScopedKey,
        CredentialKind::LoginGrant,
        CredentialKind::AdminGrant,
        CredentialKind::MachineToken,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CredentialKind::ScopedKey => "scoped_key",
            CredentialKind::LoginGrant => "login_grant",
            CredentialKind::AdminGrant => "admin_grant",
            CredentialKind::MachineToken => "machine_token",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    pub fn principal_kind(self) -> PrincipalKind {
        match self {
            CredentialKind::LoginGrant | CredentialKind::AdminGrant => PrincipalKind::User,
            CredentialKind::ScopedKey | CredentialKind::MachineToken => PrincipalKind::Service,
        }
    }

    /// The most a credential of this kind can grant; `None` allows any subset
    /// chosen at issue.
    pub fn scope_ceiling(self) -> Option<&'static [Scope]> {
        match self {
            CredentialKind::MachineToken => Some(&MACHINE_TOKEN_SCOPES),
            _ => None,
        }
    }
}

/// Credential lifecycle states. Only `Active` authenticates; the others are
/// awaiting approval or terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialState {
    Pending,
    Active,
    Revoked,
    Expired,
    Consumed,
}

impl CredentialState {
    pub const ALL: [CredentialState; 5] = [
        CredentialState::Pending,
        CredentialState::Active,
        CredentialState::Revoked,
        CredentialState::Expired,
        CredentialState::Consumed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CredentialState::Pending => "pending",
            CredentialState::Active => "active",
            CredentialState::Revoked => "revoked",
            CredentialState::Expired => "expired",
            CredentialState::Consumed => "consumed",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == name)
    }
}

/// What a machine token serves: one machine, or one single-use workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineBinding {
    Machine(MachineId),
    /// The token of a single-use workspace; its credential workspace must be one.
    SingleUseWorkspace,
}

/// A resolved credential. It never holds credential bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub kind: CredentialKind,
    pub state: CredentialState,
    pub scopes: BTreeSet<Scope>,
    /// The one workspace the credential is bound to. Later workspaces never join it.
    pub workspace: WorkspaceId,
    /// Required for a machine token, which never authenticates without it;
    /// any other kind carrying one never authenticates either.
    pub machine: Option<MachineBinding>,
}

impl Credential {
    /// An active credential with no scopes and no machine binding. A machine
    /// token built this way is unbound and denied until `on_machine` or
    /// `for_single_use_workspace` binds it.
    pub fn new(kind: CredentialKind, workspace: WorkspaceId) -> Self {
        Self {
            kind,
            state: CredentialState::Active,
            scopes: BTreeSet::new(),
            workspace,
            machine: None,
        }
    }

    pub fn with_state(mut self, state: CredentialState) -> Self {
        self.state = state;
        self
    }

    pub fn with_scopes(mut self, scopes: impl IntoIterator<Item = Scope>) -> Self {
        self.scopes = scopes.into_iter().collect();
        self
    }

    pub fn on_machine(mut self, machine: MachineId) -> Self {
        self.machine = Some(MachineBinding::Machine(machine));
        self
    }

    pub fn for_single_use_workspace(mut self) -> Self {
        self.machine = Some(MachineBinding::SingleUseWorkspace);
        self
    }

    /// Whether the credential grants `scope`: it must be listed and within the
    /// kind's ceiling, so an over-issued machine token gains nothing.
    pub fn grants(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
            && self
                .kind
                .scope_ceiling()
                .is_none_or(|ceiling| ceiling.contains(&scope))
    }
}

/// The caller of an operation: one principal and the credential it presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub id: PrincipalId,
    pub credential: Credential,
}

impl Principal {
    pub fn new(id: PrincipalId, credential: Credential) -> Self {
        Self { id, credential }
    }

    pub fn kind(&self) -> PrincipalKind {
        self.credential.kind.principal_kind()
    }
}
