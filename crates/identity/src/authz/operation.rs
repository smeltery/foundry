//! The operation catalog: each operation's scope, whether it targets the bound
//! workspace, and its row of the role and permission matrix. Names are
//! provisional pending the naming decisions.

use super::scope::{Role, Scope};

/// One cell of the role and permission matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Allow,
    Deny,
    /// Allowed with withheld data.
    Partial,
    /// Not gated by a workspace role.
    Any,
    /// Every role receives `retired`.
    Retired,
    /// The evidence does not establish it; decided as a denial.
    Unknown,
}

impl Cell {
    pub fn name(self) -> &'static str {
        match self {
            Cell::Allow => "allow",
            Cell::Deny => "deny",
            Cell::Partial => "partial",
            Cell::Any => "any",
            Cell::Retired => "retired",
            Cell::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationRule {
    /// `None` means any valid credential may call.
    pub scope: Option<Scope>,
    pub workspace_scoped: bool,
    /// Cells in `Role::ALL` order.
    cells: [Cell; 4],
}

impl OperationRule {
    pub fn cell(&self, role: Role) -> Cell {
        self.cells[role as usize]
    }

    /// The cell every role shares, when the row is uniform.
    pub fn uniform_cell(&self) -> Option<Cell> {
        let first = self.cells[0];
        self.cells
            .iter()
            .all(|cell| *cell == first)
            .then_some(first)
    }
}

macro_rules! catalog {
    ($($variant:ident $name:literal $scope:expr, $scoped:literal, [$($cell:ident),+];)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Operation {
            $($variant,)+
        }

        impl Operation {
            pub const ALL: &'static [Operation] = &[$(Operation::$variant,)+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Operation::$variant => $name,)+
                }
            }

            pub fn rule(self) -> OperationRule {
                match self {
                    $(Operation::$variant => OperationRule {
                        scope: $scope,
                        workspace_scoped: $scoped,
                        cells: [$(Cell::$cell),+],
                    },)+
                }
            }
        }
    };
}

const NONE: Option<Scope> = None;
const DEPLOYMENT_READ: Option<Scope> = Some(Scope::DeploymentRead);
const WORKSPACE_READ: Option<Scope> = Some(Scope::WorkspaceRead);
const WORKSPACE_WRITE: Option<Scope> = Some(Scope::WorkspaceWrite);
const AUDIT_READ: Option<Scope> = Some(Scope::AuditRead);
const POLICY_READ: Option<Scope> = Some(Scope::NetworkPolicyRead);
const POLICY_WRITE: Option<Scope> = Some(Scope::NetworkPolicyWrite);

// Columns: owner, admin, member, viewer.
catalog! {
    IdentitySelf "identity.self" NONE, false, [Any, Any, Any, Any];
    ServiceDeployment "service.deployment" DEPLOYMENT_READ, false, [Unknown, Unknown, Unknown, Unknown];
    ServiceApiDescription "service.api_description" NONE, false, [Any, Any, Any, Any];
    FeedbackSubmit "feedback.submit" NONE, false, [Any, Any, Any, Any];
    WorkspaceList "workspace.list" WORKSPACE_READ, false, [Allow, Allow, Allow, Allow];
    WorkspaceCreate "workspace.create" WORKSPACE_WRITE, false, [Any, Any, Any, Any];
    WorkspaceGet "workspace.get" WORKSPACE_READ, true, [Allow, Allow, Allow, Unknown];
    WorkspaceRename "workspace.rename" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
    WorkspaceDelete "workspace.delete" WORKSPACE_WRITE, true, [Allow, Deny, Deny, Deny];
    AuditList "audit.list" AUDIT_READ, true, [Allow, Allow, Partial, Unknown];
    SingleUseCreate "single_use.create" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    SingleUseRetire "single_use.retire" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
    MemberList "member.list" WORKSPACE_READ, true, [Allow, Allow, Partial, Unknown];
    MemberInvite "member.invite" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
    MemberGet "member.get" WORKSPACE_READ, true, [Allow, Allow, Allow, Unknown];
    MemberRemove "member.remove" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
    MemberSetRole "member.set_role" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
    NetworkPolicyGet "network_policy.get" POLICY_READ, true, [Unknown, Unknown, Unknown, Unknown];
    NetworkPolicyReplace "network_policy.replace" POLICY_WRITE, true, [Allow, Allow, Deny, Deny];
    SecretImportHandoff "secret_import.handoff" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    SecretImportDecision "secret_import.decision" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    SecretImportComplete "secret_import.complete" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    PairingClaim "pairing.claim" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    PairingFail "pairing.fail" WORKSPACE_WRITE, true, [Unknown, Unknown, Unknown, Unknown];
    TierList "tier.list" WORKSPACE_READ, true, [Retired, Retired, Retired, Retired];
    TierSet "tier.set" WORKSPACE_WRITE, true, [Retired, Retired, Retired, Retired];
    TierGet "tier.get" WORKSPACE_READ, true, [Retired, Retired, Retired, Retired];
    RoleOwn "role.own" WORKSPACE_READ, true, [Allow, Allow, Allow, Allow];
    SettingsList "settings.list" WORKSPACE_READ, true, [Unknown, Unknown, Unknown, Unknown];
    SettingsGet "settings.get" WORKSPACE_READ, true, [Unknown, Unknown, Unknown, Unknown];
    SettingsReplace "settings.replace" WORKSPACE_WRITE, true, [Allow, Allow, Deny, Deny];
}

impl Operation {
    /// Resolves a provisional wire name. Unknown names resolve to nothing and
    /// are denied by default.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.name() == name)
    }
}
