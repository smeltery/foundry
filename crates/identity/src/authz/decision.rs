//! The authorization decision. It evaluates the specified check order and
//! reports the first failure: authenticate, throttle, resolve the operation,
//! bind workspace and machine, scope, retirement, role, then the ownership and
//! isolation rules that need only identity state. Anything it cannot establish
//! is denied.

use super::ids::{MachineId, PrincipalId, WorkspaceId};
use super::operation::{Cell, Operation, OperationRule};
use super::organization::{Organization, Workspace, WorkspaceState};
use super::principal::{CredentialKind, CredentialState, MachineBinding, Principal, PrincipalKind};
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
    /// The single-use workspace a retirement acts on. Required for it.
    pub single_use: Option<WorkspaceId>,
    /// The member a membership change acts on. Required for non-owners.
    pub member: Option<PrincipalId>,
    /// The role a role change assigns. Required for non-owners.
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
            single_use: None,
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

    pub fn retiring(mut self, single_use: WorkspaceId) -> Self {
        self.single_use = Some(single_use);
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
    Unavailable,
}

impl ErrorClass {
    pub fn name(self) -> &'static str {
        match self {
            ErrorClass::Unauthenticated => "unauthenticated",
            ErrorClass::RateLimited { .. } => "rate_limited",
            ErrorClass::NotFound => "not_found",
            ErrorClass::Forbidden => "forbidden",
            ErrorClass::Retired => "retired",
            ErrorClass::Unavailable => "unavailable",
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
    /// The bound workspace is erasing and admits only deletion and identity.
    Erasing,
    Scope,
    Retired,
    Role,
    /// The matrix cell is unknown, or the caller is a service principal whose
    /// effective role is held under decision C1. Both default to denial.
    UndeterminedRole,
    /// An `admin_grant`, whose capability set is held under decision C1.
    HeldCredential,
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
    bind(principal, org, bound, operation, &rule, request)?;
    if bound.state == WorkspaceState::Erasing
        && !matches!(
            operation,
            Operation::WorkspaceDelete | Operation::IdentitySelf
        )
    {
        return Err(Denial::new(ErrorClass::Unavailable, Check::Erasing));
    }
    if let Some(scope) = rule.scope
        && !principal.credential.grants(scope)
    {
        return Err(Denial::forbidden(Check::Scope));
    }
    let access = gate_role(&rule, principal.credential.kind, role)?;
    check_ownership(operation, principal, bound, role, request)?;
    Ok(access)
}

/// Resolves the bound workspace and the caller's current role. Revocation is
/// read from committed state: a terminal credential, a deleted or retired
/// workspace, an unregistered machine, or a removed membership. A machine
/// token without a valid machine binding never authenticates.
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
    let binding_holds = match (credential.kind, &credential.machine) {
        (CredentialKind::MachineToken, Some(MachineBinding::Machine(machine))) => {
            workspace.has_machine(machine)
        }
        (CredentialKind::MachineToken, Some(MachineBinding::SingleUseWorkspace)) => {
            workspace.parent().is_some()
        }
        // Unbound machine tokens, and machine bindings on any other kind.
        (CredentialKind::MachineToken, None) | (_, Some(_)) => false,
        (_, None) => true,
    };
    if !binding_holds {
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
    org: &Organization,
    bound: &Workspace,
    operation: Operation,
    rule: &OperationRule,
    request: &OperationRequest,
) -> Result<(), Denial> {
    let outside = Denial::new(ErrorClass::NotFound, Check::WorkspaceBinding);
    let target = request
        .workspace
        .as_ref()
        .or(rule.workspace_scoped.then_some(&bound.id));
    if target.is_some_and(|target| *target != bound.id) {
        return Err(outside);
    }
    // A single-use workspace is separate; only its paying parent retires it.
    if operation == Operation::SingleUseRetire {
        let parent = request
            .single_use
            .as_ref()
            .and_then(|single_use| org.workspace(single_use))
            .and_then(Workspace::parent);
        if parent != Some(&bound.id) {
            return Err(outside);
        }
    }
    if let Some(machine) = &request.machine {
        let confined_elsewhere = matches!(
            &principal.credential.machine,
            Some(MachineBinding::Machine(own)) if own != machine
        );
        if confined_elsewhere || !bound.has_machine(machine) {
            return Err(Denial::new(ErrorClass::NotFound, Check::MachineBinding));
        }
    }
    Ok(())
}

fn gate_role(
    rule: &OperationRule,
    kind: CredentialKind,
    role: Option<Role>,
) -> Result<Access, Denial> {
    let cell = match (rule.uniform_cell(), role) {
        (Some(cell @ (Cell::Any | Cell::Retired)), _) => cell,
        (_, Some(role)) => rule.cell(role),
        (_, None) => Cell::Unknown,
    };
    // Decision C1 holds the `admin_grant` capability set. Until it settles, the
    // grant reaches only rows any valid credential may call (no scope) and
    // still receives `retired`; every other row is denied.
    if kind == CredentialKind::AdminGrant && rule.scope.is_some() && cell != Cell::Retired {
        return Err(Denial::forbidden(Check::HeldCredential));
    }
    match cell {
        Cell::Allow | Cell::Any => Ok(Access::Full),
        Cell::Partial => Ok(Access::Partial),
        Cell::Retired => Err(Denial::new(ErrorClass::Retired, Check::Retired)),
        Cell::Deny => Err(Denial::forbidden(Check::Role)),
        Cell::Unknown => Err(Denial::forbidden(Check::UndeterminedRole)),
    }
}

/// Owner-only membership changes, and the rule that a workspace-bound login
/// grant cannot create another workspace. A non-owner must name the target
/// member, and for a role change the new role, so the owner rule is never
/// skipped for lack of input.
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
        .is_none_or(|member| bound.role_of(member) == Some(Role::Owner));
    let grants_owner = operation == Operation::MemberSetRole
        && request.new_role.is_none_or(|role| role == Role::Owner);
    if target_is_owner || grants_owner {
        return Err(Denial {
            cause: Some(Cause::OwnerRequired),
            ..Denial::forbidden(Check::Ownership)
        });
    }
    Ok(())
}
