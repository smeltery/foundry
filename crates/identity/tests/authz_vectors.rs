//! Replays the neutral identity vectors and catalog in `tests/identity`
//! against the authorization decision. Every vector with a caller is
//! replayed; the expected decision comes from the vector's own expectation.
use std::fs;
use std::path::{Path, PathBuf};

use foundry_identity::authz::*;
use serde_json::Value;

const CALLER: &str = "caller";
const MACHINE: &str = "m-1";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/identity")
}

fn read(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

fn catalog() -> Value {
    read(&fixtures().join("catalog.json"))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn names(values: &Value) -> Vec<&str> {
    values
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect()
}

/// Comparable form of a decision: access, or class name and cause name.
type Outcome = Result<Access, (String, Option<String>)>;

fn outcome(decision: Result<Access, Denial>) -> Outcome {
    decision.map_err(|denial| {
        (
            denial.class.name().to_owned(),
            denial.cause.map(|cause| cause.name().to_owned()),
        )
    })
}

/// Builds the principal, identity state and request a vector describes.
fn arrange(case: &Value) -> (Option<Principal>, Organization, OperationRequest) {
    let caller = &case["caller"];
    let given = case.get("given").cloned().unwrap_or(Value::Null);
    let body = &case["request"];
    let bound = WorkspaceId::new(text(caller, "workspace").expect("caller workspace"));
    let kind = CredentialKind::from_name(text(caller, "kind").expect("kind")).expect("known kind");
    let scopes = names(&caller["scopes"])
        .into_iter()
        .map(|name| Scope::from_name(name).expect("known scope"));
    let state = text(caller, "state").unwrap_or("active");
    let principal = (state != "missing").then(|| {
        let state = CredentialState::from_name(state).expect("known state");
        let mut credential = Credential::new(kind, bound.clone())
            .with_scopes(scopes)
            .with_state(state);
        // Machine tokens always serve one machine; an unbound one is denied.
        if kind == CredentialKind::MachineToken {
            credential = credential.on_machine(MachineId::new(MACHINE));
        }
        Principal::new(PrincipalId::new(CALLER), credential)
    });

    let mut workspace = Workspace::new(bound.clone()).with_machine(MachineId::new(MACHINE));
    if let Some(role) = text(caller, "role") {
        workspace = workspace.with_member(PrincipalId::new(CALLER), role_named(role));
    }
    if let (Some(user), Some(role)) = (text(body, "user"), text(&given, "target_role")) {
        workspace = workspace.with_member(PrincipalId::new(user), role_named(role));
    }
    let mut org = Organization::new().with_workspace(workspace);

    let mut request = OperationRequest::new(text(case, "operation").expect("operation"));
    // The single-use workspace exists with the parent the vector gives, or the
    // caller's workspace; authorize decides from that relationship.
    if let Some(single_use) = text(body, "single_use") {
        let single_use = WorkspaceId::new(single_use);
        let parent = text(&given, "single_use_parent").map_or(bound.clone(), WorkspaceId::new);
        org = org.with_workspace(Workspace::new(single_use.clone()).single_use_of(parent));
        request = request.retiring(single_use);
    }
    if let Some(target) = text(body, "workspace").or(text(&given, "workspace")) {
        let target = WorkspaceId::new(target);
        if target != bound {
            // The other workspace exists and the caller even owns it there;
            // the binding alone must hide it.
            org = org.with_workspace(
                Workspace::new(target.clone()).with_member(PrincipalId::new(CALLER), Role::Owner),
            );
        }
        request = request.in_workspace(target);
    }
    if let Some(user) = text(body, "user") {
        request = request.for_member(PrincipalId::new(user));
    }
    if let Some(role) = text(body, "role") {
        request = request.assigning(role_named(role));
    }
    if given.get("rate_limited") == Some(&Value::Bool(true)) {
        let seconds = case["expect"]["retry_after_seconds"]
            .as_u64()
            .expect("throttled vectors carry a delay");
        request = request.throttled(u32::try_from(seconds).expect("delay fits"));
    }
    (principal, org, request)
}

fn role_named(name: &str) -> Role {
    Role::from_name(name).unwrap_or_else(|| panic!("unknown role {name}"))
}

/// The decision a vector requires. Gate classes are authorization outcomes;
/// any later error belongs to the operation handler, so authorization allows.
fn expected(case: &Value, catalog: &Value) -> Outcome {
    let expect = &case["expect"];
    let denial =
        |class: &str, cause: Option<&str>| Err((class.to_owned(), cause.map(str::to_owned)));
    match (text(expect, "error"), text(expect, "cause")) {
        (Some(class @ ("unauthenticated" | "rate_limited" | "retired" | "not_found")), None) => {
            denial(class, None)
        }
        (Some("forbidden"), cause @ (None | Some("owner_required" | "grant_workspace_bound"))) => {
            denial("forbidden", cause)
        }
        _ => {
            let operation = text(case, "operation").expect("operation");
            let cell = text(&case["caller"], "role")
                .and_then(|role| text(&catalog["operations"][operation]["roles"], role));
            Ok(if cell == Some("partial") {
                Access::Partial
            } else {
                Access::Full
            })
        }
    }
}

#[test]
fn every_vector_with_a_caller_replays_its_authorization_outcome() {
    let catalog = catalog();
    let mut paths: Vec<PathBuf> = fs::read_dir(fixtures().join("vectors"))
        .expect("vector directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    let mut replayed = 0;
    let mut failures = Vec::new();
    for path in &paths {
        for case in read(path)["cases"].as_array().expect("cases") {
            if case.get("caller").is_none() {
                continue;
            }
            let (principal, org, request) = arrange(case);
            let decision = authorize(principal.as_ref(), &org, &request);
            if let Err(Denial {
                class:
                    ErrorClass::RateLimited {
                        retry_after_seconds,
                    },
                ..
            }) = decision
            {
                assert_eq!(
                    Some(u64::from(retry_after_seconds)),
                    case["expect"]["retry_after_seconds"].as_u64()
                );
            }
            let (actual, wanted) = (outcome(decision), expected(case, &catalog));
            if actual != wanted {
                failures.push(format!("{}: got {actual:?}, want {wanted:?}", case["name"]));
            }
            replayed += 1;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Guards against silently skipping files or cases. Update deliberately
    // when vectors with a caller are added or removed.
    assert_eq!(replayed, 67, "replayed {replayed} cases");
}

/// Guards `full_matrix_replays_for_users_and_service_principals`: that test
/// derives its expectations from the Rust catalog, so this check pins the Rust
/// catalog to `catalog.json` and the matrix there.
#[test]
fn operation_catalog_matches_the_fixture_catalog() {
    let catalog = catalog();
    let roles = names(&catalog["roles"]);
    assert_eq!(roles, Role::ALL.map(Role::name));
    assert_eq!(names(&catalog["scopes"]), Scope::ALL.map(Scope::name));
    let operations = catalog["operations"].as_object().expect("operations");
    let mut gated = 0;
    for (name, entry) in operations
        .iter()
        .filter(|(_, entry)| entry.get("roles").is_some())
    {
        gated += 1;
        let operation = Operation::from_name(name).unwrap_or_else(|| panic!("missing {name}"));
        let rule = operation.rule();
        let scope = text(entry, "scope").expect("scope");
        assert_eq!(
            rule.scope.map_or("none", Scope::name),
            scope,
            "{name} scope"
        );
        assert_eq!(
            Some(rule.workspace_scoped),
            entry["workspace_scoped"].as_bool(),
            "{name}"
        );
        for role in Role::ALL {
            assert_eq!(
                Some(rule.cell(role).name()),
                text(&entry["roles"], role.name()),
                "{name}"
            );
        }
    }
    assert_eq!(gated, Operation::ALL.len());

    let kinds = catalog["credential_kinds"]
        .as_object()
        .expect("credential kinds");
    assert_eq!(kinds.len(), CredentialKind::ALL.len());
    for (name, entry) in kinds {
        let kind = CredentialKind::from_name(name).unwrap_or_else(|| panic!("missing {name}"));
        let principal = match kind.principal_kind() {
            PrincipalKind::User => "user",
            PrincipalKind::Service => "service",
        };
        assert_eq!(Some(principal), text(entry, "principal"), "{name}");
        let ceiling = kind
            .scope_ceiling()
            .map(|scopes| scopes.iter().map(|scope| scope.name()).collect());
        assert_eq!(ceiling, entry.get("scopes").map(names), "{name}");
    }
}

/// Every matrix cell, for a user holding every scope and for a service
/// principal, whose effective role is held and therefore denied unless the
/// row is not role-gated.
#[test]
fn full_matrix_replays_for_users_and_service_principals() {
    let bound = WorkspaceId::new("ws-a");
    let single_use = WorkspaceId::new("su-1");
    // Every input an operation may require: a non-owner target member, a
    // non-owner role and a single-use child of the caller's workspace.
    let request = |operation: Operation| {
        OperationRequest::new(operation.name())
            .for_member(PrincipalId::new("user-z"))
            .assigning(Role::Member)
            .retiring(single_use.clone())
    };
    let child = Workspace::new(single_use.clone()).single_use_of(bound.clone());
    for operation in Operation::ALL.iter().copied() {
        let rule = operation.rule();
        for role in Role::ALL {
            let user = Principal::new(
                PrincipalId::new(CALLER),
                Credential::new(CredentialKind::LoginGrant, bound.clone()).with_scopes(Scope::ALL),
            );
            let org = Organization::new()
                .with_workspace(Workspace::new(bound.clone()).with_member(user.id.clone(), role))
                .with_workspace(child.clone());
            let wanted = match rule.cell(role) {
                _ if operation == Operation::WorkspaceCreate => Err(Check::Isolation),
                Cell::Allow | Cell::Any => Ok(Access::Full),
                Cell::Partial => Ok(Access::Partial),
                Cell::Retired => Err(Check::Retired),
                Cell::Deny => Err(Check::Role),
                Cell::Unknown => Err(Check::UndeterminedRole),
            };
            let actual =
                authorize(Some(&user), &org, &request(operation)).map_err(|denial| denial.check);
            assert_eq!(actual, wanted, "{} as {}", operation.name(), role.name());
        }

        let service = Principal::new(
            PrincipalId::new("service-a"),
            Credential::new(CredentialKind::ScopedKey, bound.clone()).with_scopes(Scope::ALL),
        );
        let org = Organization::new()
            .with_workspace(Workspace::new(bound.clone()))
            .with_workspace(child.clone());
        let wanted = match rule.uniform_cell() {
            Some(Cell::Any) => Ok(Access::Full),
            Some(Cell::Retired) => Err(Check::Retired),
            _ => Err(Check::UndeterminedRole),
        };
        let actual =
            authorize(Some(&service), &org, &request(operation)).map_err(|denial| denial.check);
        assert_eq!(actual, wanted, "{} as service", operation.name());
    }
}
