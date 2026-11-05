//! The authorization decision. It evaluates the specified check order and
//! reports the first failure: authenticate, throttle, resolve the operation,
//! bind workspace and machine, scope, retirement, role, then the ownership and
//! isolation rules that need only identity state.

use super::ids::{MachineId, PrincipalId, WorkspaceId};
use super::operation::{Cell, Operation, OperationRule};
use super::organization::{Organization, Workspace};
use super::principal::{CredentialKind, CredentialState, Principal, PrincipalKind};
use super::scope::Role;

/// What the caller asks to do. Body validation and state rules stay with the
/// operation handler; this carries only what authorization reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRequest {
    /// Provisional wire name; unknown names are denied.
    pub operation: String,
    /// Target workspace. Workspace-scoped operations default to the binding.
    pub workspace: Option<WorkspaceId>,
    pub machine: Option<MachineId>,
    /// The member a membership operation acts on.
    pub member: Option<PrincipalId>,
    /// The role a role change assigns.
    pub new_role: Option<Role>,
    /// The rate limiter's verdict for this request, when it throttles.
    pub retry_after_seconds: Option<u32>,
}

impl OperationRequest {
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            workspace: None,
            machine: None,
            member: None,
            new_role: None,
            retry_after_seconds: None,
        }
    }

    pub fn in_workspace(mut self, workspace: WorkspaceId) -> Self {
        self.workspace = Some(workspace);
        self
    }

    pub fn on_machine(mut self, machine: MachineId) -> Self {
        self.machine = Some(machine);
        self
    }

    pub fn for_member(mut self, member: PrincipalId) -> Self {
        self.member = Some(member);
        self
    }

    pub fn assigning(mut self, role: Role) -> Self {
        self.new_role = Some(role);
        self
    }

    pub fn throttled(mut self, retry_after_seconds: u32) -> Self {
        self.retry_after_seconds = Some(retry_after_seconds);
        self
    }
}

/// An allowed request, and whether the handler must withhold data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Full,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Unauthenticated,
    RateLimited { retry_after_seconds: u32 },
    NotFound,
    Forbidden,
    Retired,
}

impl ErrorClass {
    pub fn name(self) -> &'static str {
        match self {
            ErrorClass::Unauthenticated => "unauthenticated",
            ErrorClass::RateLimited { .. } => "rate_limited",
            ErrorClass::NotFound => "not_found",
            ErrorClass::Forbidden => "forbidden",
            ErrorClass::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    OwnerRequired,
    GrantWorkspaceBound,
}

impl Cause {
    pub fn name(self) -> &'static str {
        match self {
            Cause::OwnerRequired => "owner_required",
            Cause::GrantWorkspaceBound => "grant_workspace_bound",
        }
    }
}

/// The check that refused the request. Not a wire field; it lets callers log
/// and tests assert which rule applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Authentication,
    Throttle,
    UnknownOperation,
    WorkspaceBinding,
    MachineBinding,
    Scope,
    Retired,
    Role,
    /// The matrix cell is unknown, or the caller is a service principal whose
    /// effective role is held under decision C1. Both default to denial.
    UndeterminedRole,
    Ownership,
    Isolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    pub class: ErrorClass,
    pub cause: Option<Cause>,
    pub check: Check,
}

impl Denial {
    fn new(class: ErrorClass, check: Check) -> Self {
        Self {
            class,
            cause: None,
            check,
        }
    }

    fn forbidden(check: Check) -> Self {
        Self::new(ErrorClass::Forbidden, check)
    }
}

/// Decides one request. `principal` is `None` when no credential was presented.
/// The decision is pure: it reads only its arguments and caches nothing.
pub fn authorize(
    principal: Option<&Principal>,
    org: &Organization,
    request: &OperationRequest,
) -> Result<Access, Denial> {
    let principal = principal.ok_or(Denial::new(
        ErrorClass::Unauthenticated,
        Check::Authentication,
    ))?;
    let (bound, role) = authenticate(principal, org)?;
    if let Some(retry_after_seconds) = request.retry_after_seconds {
        return Err(Denial::new(
            ErrorClass::RateLimited {
                retry_after_seconds,
            },
            Check::Throttle,
        ));
    }
    let operation = Operation::from_name(&request.operation)
        .ok_or(Denial::forbidden(Check::UnknownOperation))?;
    let rule = operation.rule();
    bind(principal, bound, &rule, request)?;
    if let Some(scope) = rule.scope
        && !principal.credential.grants(scope)
    {
        return Err(Denial::forbidden(Check::Scope));
    }
    let access = gate_role(&rule, role)?;
    check_ownership(operation, principal, bound, role, request)?;
    Ok(access)
}

/// Resolves the bound workspace and the caller's current role. Revocation is
/// read from committed state: a terminal credential, a deleted or retired
/// workspace, an unregistered machine, or a removed membership.
fn authenticate<'a>(
    principal: &Principal,
    org: &'a Organization,
) -> Result<(&'a Workspace, Option<Role>), Denial> {
    let revoked = Denial::new(ErrorClass::Unauthenticated, Check::Authentication);
    let credential = &principal.credential;
    if credential.state != CredentialState::Active {
        return Err(revoked);
    }
    let workspace = org
        .workspace(&credential.workspace)
        .filter(|workspace| workspace.state.keeps_credentials())
        .ok_or(revoked)?;
    if let Some(machine) = &credential.machine
        && !workspace.has_machine(machine)
    {
        return Err(revoked);
    }
    match principal.kind() {
        PrincipalKind::User => {
            let role = workspace.role_of(&principal.id).ok_or(revoked)?;
            Ok((workspace, Some(role)))
        }
        PrincipalKind::Service => Ok((workspace, None)),
    }
}

/// Targets outside the binding answer `not_found`, so a bound credential
/// cannot probe for other workspaces or machines.
fn bind(
    principal: &Principal,
    bound: &Workspace,
    rule: &OperationRule,
    request: &OperationRequest,
) -> Result<(), Denial> {
    let target = request
        .workspace
        .as_ref()
        .or(rule.workspace_scoped.then_some(&bound.id));
    if target.is_some_and(|target| *target != bound.id) {
        return Err(Denial::new(ErrorClass::NotFound, Check::WorkspaceBinding));
    }
    if let Some(machine) = &request.machine {
        let confined_elsewhere = principal
            .credential
            .machine
            .as_ref()
            .is_some_and(|own| own != machine);
        if confined_elsewhere || !bound.has_machine(machine) {
            return Err(Denial::new(ErrorClass::NotFound, Check::MachineBinding));
        }
    }
    Ok(())
}

fn gate_role(rule: &OperationRule, role: Option<Role>) -> Result<Access, Denial> {
    let cell = match (rule.uniform_cell(), role) {
        (Some(cell @ (Cell::Any | Cell::Retired)), _) => cell,
        (_, Some(role)) => rule.cell(role),
        (_, None) => Cell::Unknown,
    };
    match cell {
        Cell::Allow | Cell::Any => Ok(Access::Full),
        Cell::Partial => Ok(Access::Partial),
        Cell::Retired => Err(Denial::new(ErrorClass::Retired, Check::Retired)),
        Cell::Deny => Err(Denial::forbidden(Check::Role)),
        Cell::Unknown => Err(Denial::forbidden(Check::UndeterminedRole)),
    }
}

/// Owner-only membership changes, and the rule that a workspace-bound login
/// grant cannot create another workspace.
fn check_ownership(
    operation: Operation,
    principal: &Principal,
    bound: &Workspace,
    role: Option<Role>,
    request: &OperationRequest,
) -> Result<(), Denial> {
    if operation == Operation::WorkspaceCreate
        && principal.credential.kind == CredentialKind::LoginGrant
    {
        return Err(Denial {
            cause: Some(Cause::GrantWorkspaceBound),
            ..Denial::forbidden(Check::Isolation)
        });
    }
    if !matches!(
        operation,
        Operation::MemberRemove | Operation::MemberSetRole
    ) || role == Some(Role::Owner)
    {
        return Ok(());
    }
    let target_is_owner = request
        .member
        .as_ref()
        .and_then(|member| bound.role_of(member))
        == Some(Role::Owner);
    let grants_owner =
        operation == Operation::MemberSetRole && request.new_role == Some(Role::Owner);
    if target_is_owner || grants_owner {
        return Err(Denial {
            cause: Some(Cause::OwnerRequired),
            ..Denial::forbidden(Check::Ownership)
        });
    }
    Ok(())
}
