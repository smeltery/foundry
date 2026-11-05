//! Required inputs and bindings: owner-rule targets, machine and single-use
//! token bindings, single-use parentage, erasing workspaces and held admin grants.
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
fn non_owners_must_name_the_target_and_role() {
    let admin = Principal::new(
        user("user-b"),
        Credential::new(CredentialKind::LoginGrant, ws("ws-a"))
            .with_scopes([Scope::WorkspaceWrite]),
    );
    let incomplete = [
        OperationRequest::new("member.remove"),
        OperationRequest::new("member.set_role"),
        OperationRequest::new("member.set_role").assigning(Role::Member),
        OperationRequest::new("member.set_role").for_member(user("user-c")),
    ];
    for request in incomplete {
        let denial = authorize(Some(&admin), &org(), &request).unwrap_err();
        assert_eq!(denial.cause, Some(Cause::OwnerRequired), "{request:?}");
        assert_eq!(denial.check, Check::Ownership, "{request:?}");
        // Owners pass; the handler validates the body.
        assert_eq!(
            check(&login(), &org(), request.clone()),
            Ok(Access::Full),
            "{request:?}"
        );
    }
}

#[test]
fn unbound_and_misbound_machine_tokens_never_authenticate() {
    let unbound = Principal::new(
        user("machine-principal"),
        Credential::new(CredentialKind::MachineToken, ws("ws-a")).with_scopes(Scope::ALL),
    );
    let foreign = Principal::new(
        user("machine-principal"),
        Credential::new(CredentialKind::MachineToken, ws("ws-a"))
            .with_scopes(Scope::ALL)
            .on_machine(machine("m-3")),
    );
    // A single-use binding needs a single-use workspace.
    let not_single_use = Principal::new(
        user("machine-principal"),
        Credential::new(CredentialKind::MachineToken, ws("ws-a"))
            .with_scopes(Scope::ALL)
            .for_single_use_workspace(),
    );
    // Only machine tokens may carry a machine binding.
    let bound_key = Principal::new(
        user("service-a"),
        Credential::new(CredentialKind::ScopedKey, ws("ws-a")).on_machine(machine("m-1")),
    );
    for principal in [unbound, foreign, not_single_use, bound_key] {
        let denial = authorize(
            Some(&principal),
            &org(),
            &OperationRequest::new("identity.self"),
        )
        .unwrap_err();
        assert_eq!(denial.class, ErrorClass::Unauthenticated, "{principal:?}");
    }
}

#[test]
fn single_use_tokens_reach_only_their_own_workspace() {
    let org = org().with_workspace(
        Workspace::new(ws("su-1"))
            .single_use_of(ws("ws-a"))
            .with_machine(machine("m-9")),
    );
    let token = Principal::new(
        user("trial-principal"),
        Credential::new(CredentialKind::MachineToken, ws("su-1"))
            .with_scopes([Scope::SessionsRun])
            .for_single_use_workspace(),
    );
    let on = |target: &str| OperationRequest::new("identity.self").on_machine(machine(target));
    assert_eq!(check(&token, &org, on("m-9")), Ok(Access::Full));
    assert_eq!(check(&token, &org, on("m-1")), Err(Check::MachineBinding));
    let parent = OperationRequest::new("identity.self").in_workspace(ws("ws-a"));
    assert_eq!(check(&token, &org, parent), Err(Check::WorkspaceBinding));
}

#[test]
fn only_the_paying_parent_retires_a_single_use_workspace() {
    let org = org()
        .with_workspace(Workspace::new(ws("su-1")).single_use_of(ws("ws-a")))
        .with_workspace(Workspace::new(ws("su-9")).single_use_of(ws("ws-b")));
    let retire = || OperationRequest::new("single_use.retire");
    assert_eq!(
        check(&login(), &org, retire().retiring(ws("su-1"))),
        Ok(Access::Full)
    );
    for request in [
        retire().retiring(ws("su-9")),
        retire().retiring(ws("su-unknown")),
        // A regular workspace has no parent.
        retire().retiring(ws("ws-a")),
        retire(),
    ] {
        assert_eq!(
            check(&login(), &org, request.clone()),
            Err(Check::WorkspaceBinding),
            "{request:?}"
        );
    }
}

#[test]
fn an_erasing_workspace_admits_only_deletion_and_identity() {
    let erasing = Organization::new()
        .with_workspace(
            Workspace::new(ws("ws-a"))
                .with_state(WorkspaceState::Erasing)
                .with_member(user("user-a"), Role::Owner),
        )
        .with_workspace(Workspace::new(ws("su-1")).single_use_of(ws("ws-a")));
    for operation in Operation::ALL.iter().copied() {
        let request = OperationRequest::new(operation.name()).retiring(ws("su-1"));
        let decision = authorize(Some(&login()), &erasing, &request);
        match operation {
            Operation::WorkspaceDelete | Operation::IdentitySelf => {
                assert_eq!(decision, Ok(Access::Full), "{}", operation.name());
            }
            _ => {
                let denial = decision.unwrap_err();
                assert_eq!(
                    denial.class,
                    ErrorClass::Unavailable,
                    "{}",
                    operation.name()
                );
                assert_eq!(denial.check, Check::Erasing, "{}", operation.name());
            }
        }
    }
}

#[test]
fn admin_grants_reach_only_scope_free_rows_until_their_capabilities_are_decided() {
    let grant = Principal::new(
        user("user-a"),
        Credential::new(CredentialKind::AdminGrant, ws("ws-a")).with_scopes(Scope::ALL),
    );
    let mut allowed = Vec::new();
    for operation in Operation::ALL.iter().copied() {
        let rule = operation.rule();
        let wanted = match (rule.scope, rule.uniform_cell()) {
            (None, _) => Ok(Access::Full),
            (_, Some(Cell::Retired)) => Err(Check::Retired),
            _ => Err(Check::HeldCredential),
        };
        if wanted.is_ok() {
            allowed.push(operation);
        }
        let request = OperationRequest::new(operation.name())
            .for_member(user("user-c"))
            .assigning(Role::Member)
            .retiring(ws("su-1"));
        let org = org().with_workspace(Workspace::new(ws("su-1")).single_use_of(ws("ws-a")));
        assert_eq!(check(&grant, &org, request), wanted, "{}", operation.name());
    }
    // Pinned so a catalog change cannot silently widen the grant.
    assert_eq!(
        allowed,
        [
            Operation::IdentitySelf,
            Operation::ServiceApiDescription,
            Operation::FeedbackSubmit
        ]
    );
}
