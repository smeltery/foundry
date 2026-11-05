//! Workspace isolation, machine confinement, revocation, ownership and
//! default denial, beyond what the shared vectors encode.
use foundry_identity::authz::*;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

fn user(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

fn machine(id: &str) -> MachineId {
    MachineId::new(id)
}

/// A user login bound to `ws-a` with every scope.
fn login() -> Principal {
    Principal::new(
        user("user-a"),
        Credential::new(CredentialKind::LoginGrant, ws("ws-a")).with_scopes(Scope::ALL),
    )
}

/// A machine token for `m-1` in `ws-a`, issued with every scope.
fn machine_token() -> Principal {
    Principal::new(
        user("machine-principal"),
        Credential::new(CredentialKind::MachineToken, ws("ws-a"))
            .with_scopes(Scope::ALL)
            .on_machine(machine("m-1")),
    )
}

/// `user-a` owns both workspaces; `user-b` is an admin of `ws-a`.
fn org() -> Organization {
    Organization::new()
        .with_workspace(
            Workspace::new(ws("ws-a"))
                .with_member(user("user-a"), Role::Owner)
                .with_member(user("user-b"), Role::Admin)
                .with_member(user("user-c"), Role::Member)
                .with_machine(machine("m-1"))
                .with_machine(machine("m-2")),
        )
        .with_workspace(
            Workspace::new(ws("ws-b"))
                .with_member(user("user-a"), Role::Owner)
                .with_machine(machine("m-3")),
        )
}

fn check(
    principal: &Principal,
    org: &Organization,
    request: OperationRequest,
) -> Result<Access, Check> {
    authorize(Some(principal), org, &request).map_err(|denial| denial.check)
}

#[test]
fn every_operation_on_another_workspace_is_not_found() {
    // The caller owns ws-b too, yet a credential bound to ws-a never reaches it.
    for operation in Operation::ALL.iter().copied() {
        let request = OperationRequest::new(operation.name()).in_workspace(ws("ws-b"));
        let denial = authorize(Some(&login()), &org(), &request).expect_err(operation.name());
        assert_eq!(denial.class, ErrorClass::NotFound, "{}", operation.name());
        assert_eq!(
            denial.check,
            Check::WorkspaceBinding,
            "{}",
            operation.name()
        );
    }
    let unknown = OperationRequest::new("workspace.rename").in_workspace(ws("ws-unknown"));
    assert_eq!(
        check(&login(), &org(), unknown),
        Err(Check::WorkspaceBinding)
    );
}

#[test]
fn creating_a_workspace_never_extends_existing_credentials() {
    let key = Principal::new(
        user("service-a"),
        Credential::new(CredentialKind::ScopedKey, ws("ws-a")).with_scopes([Scope::WorkspaceRead]),
    );
    let later = org().with_workspace(Workspace::new(ws("ws-new")));
    let request = OperationRequest::new("workspace.get").in_workspace(ws("ws-new"));
    assert_eq!(check(&key, &later, request), Err(Check::WorkspaceBinding));
}

#[test]
fn machine_tokens_are_confined_to_their_own_machine() {
    let cases = [
        ("m-1", Ok(Access::Full)),
        ("m-2", Err(Check::MachineBinding)),
        ("m-3", Err(Check::MachineBinding)),
        ("m-unregistered", Err(Check::MachineBinding)),
    ];
    for (target, wanted) in cases {
        let request = OperationRequest::new("identity.self").on_machine(machine(target));
        assert_eq!(check(&machine_token(), &org(), request), wanted, "{target}");
    }
    let denial = authorize(
        Some(&machine_token()),
        &org(),
        &OperationRequest::new("identity.self").on_machine(machine("m-2")),
    )
    .unwrap_err();
    assert_eq!(denial.class, ErrorClass::NotFound);
}

#[test]
fn workspace_credentials_reach_only_machines_in_their_workspace() {
    for (target, wanted) in [
        ("m-2", Ok(Access::Full)),
        ("m-3", Err(Check::MachineBinding)),
    ] {
        let request = OperationRequest::new("identity.self").on_machine(machine(target));
        assert_eq!(check(&login(), &org(), request), wanted, "{target}");
    }
}

#[test]
fn machine_tokens_gain_nothing_beyond_the_fixed_scope_set() {
    let token = machine_token();
    assert!(token.credential.grants(Scope::SessionsRun));
    assert!(!token.credential.grants(Scope::WorkspaceWrite));
    for operation in [
        "workspace.create",
        "workspace.list",
        "network_policy.replace",
        "audit.list",
    ] {
        assert_eq!(
            check(&token, &org(), OperationRequest::new(operation)),
            Err(Check::Scope),
            "{operation}"
        );
    }
}

#[test]
fn revocation_applies_to_the_next_decision() {
    let rename = || OperationRequest::new("workspace.rename");
    for state in [
        CredentialState::Pending,
        CredentialState::Revoked,
        CredentialState::Expired,
        CredentialState::Consumed,
    ] {
        let mut principal = login();
        principal.credential.state = state;
        // Revocation is reported ahead of throttling, binding and unknown operations.
        let request = OperationRequest::new("no.such")
            .in_workspace(ws("ws-b"))
            .throttled(5);
        let denial = authorize(Some(&principal), &org(), &request).unwrap_err();
        assert_eq!(denial.class, ErrorClass::Unauthenticated, "{state:?}");
    }

    let removed = Organization::new().with_workspace(Workspace::new(ws("ws-a")));
    assert_eq!(
        check(&login(), &removed, rename()),
        Err(Check::Authentication)
    );

    for state in [WorkspaceState::Deleted, WorkspaceState::Retired] {
        let gone = Organization::new().with_workspace(
            Workspace::new(ws("ws-a"))
                .with_state(state)
                .with_member(user("user-a"), Role::Owner),
        );
        assert_eq!(
            check(&login(), &gone, rename()),
            Err(Check::Authentication),
            "{state:?}"
        );
    }
    assert_eq!(
        check(&login(), &Organization::new(), rename()),
        Err(Check::Authentication)
    );

    let erasing = Organization::new().with_workspace(
        Workspace::new(ws("ws-a"))
            .with_state(WorkspaceState::Erasing)
            .with_member(user("user-a"), Role::Owner),
    );
    let delete = OperationRequest::new("workspace.delete");
    assert_eq!(check(&login(), &erasing, delete), Ok(Access::Full));

    // A machine revoked from its workspace takes its token with it.
    let revoked_machine = Organization::new().with_workspace(Workspace::new(ws("ws-a")));
    let request = OperationRequest::new("identity.self");
    assert_eq!(
        check(&machine_token(), &revoked_machine, request),
        Err(Check::Authentication)
    );
}

#[test]
fn role_changes_apply_to_the_next_decision() {
    let with_role = |role| {
        Organization::new()
            .with_workspace(Workspace::new(ws("ws-a")).with_member(user("user-a"), role))
    };
    let rename = || OperationRequest::new("workspace.rename");
    assert_eq!(
        check(&login(), &with_role(Role::Admin), rename()),
        Ok(Access::Full)
    );
    assert_eq!(
        check(&login(), &with_role(Role::Member), rename()),
        Err(Check::Role)
    );
}

#[test]
fn unknown_operations_are_denied_by_default() {
    for name in [
        "",
        "workspace.GET",
        "workspace.get ",
        "session.create",
        "machines.use",
        "tier",
        "admin.*",
    ] {
        let denial = authorize(Some(&login()), &org(), &OperationRequest::new(name)).unwrap_err();
        assert_eq!(denial.class, ErrorClass::Forbidden, "{name:?}");
        assert_eq!(denial.check, Check::UnknownOperation, "{name:?}");
    }
    // Without a credential the catalog stays hidden.
    let anonymous = authorize(None, &org(), &OperationRequest::new("session.create")).unwrap_err();
    assert_eq!(anonymous.class, ErrorClass::Unauthenticated);
}

#[test]
fn only_owners_change_ownership() {
    let admin = Principal::new(
        user("user-b"),
        Credential::new(CredentialKind::LoginGrant, ws("ws-a"))
            .with_scopes([Scope::WorkspaceWrite]),
    );
    let owner_required = Err(Check::Ownership);
    let cases = [
        ("member.remove", "user-a", None, owner_required),
        (
            "member.set_role",
            "user-a",
            Some(Role::Admin),
            owner_required,
        ),
        (
            "member.set_role",
            "user-c",
            Some(Role::Owner),
            owner_required,
        ),
        (
            "member.set_role",
            "user-c",
            Some(Role::Admin),
            Ok(Access::Full),
        ),
        ("member.remove", "user-c", None, Ok(Access::Full)),
        // A non-member target is the handler's not_member answer, not a denial here.
        ("member.remove", "user-z", None, Ok(Access::Full)),
    ];
    for (operation, target, role, wanted) in cases {
        let mut request = OperationRequest::new(operation).for_member(user(target));
        if let Some(role) = role {
            request = request.assigning(role);
        }
        assert_eq!(
            check(&admin, &org(), request.clone()),
            wanted,
            "admin {operation} {target}"
        );
        assert_eq!(
            check(&login(), &org(), request),
            Ok(Access::Full),
            "owner {operation} {target}"
        );
    }
    let denial = authorize(
        Some(&admin),
        &org(),
        &OperationRequest::new("member.remove").for_member(user("user-a")),
    )
    .unwrap_err();
    assert_eq!(
        (denial.class, denial.cause),
        (ErrorClass::Forbidden, Some(Cause::OwnerRequired))
    );
}

#[test]
fn throttling_is_reported_before_binding_and_scope() {
    let request = OperationRequest::new("workspace.get")
        .in_workspace(ws("ws-b"))
        .throttled(30);
    let denial = authorize(Some(&login()), &org(), &request).unwrap_err();
    assert_eq!(
        denial.class,
        ErrorClass::RateLimited {
            retry_after_seconds: 30
        }
    );
}
