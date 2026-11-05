//! Workspace create, list, get, rename and delete through the decision core.
mod org_fixture;

use std::time::Duration;

use foundry_identity::authz::{Check, Role, Scope};
use foundry_identity::org::OrgError;
use foundry_identity::store::{CredentialStore, StoreError};
use org_fixture::{Fixture, failure, key, login, who, ws};

const READ: &[Scope] = &[Scope::WorkspaceRead];
const WRITE: &[Scope] = &[Scope::WorkspaceWrite];

fn team() -> Fixture {
    let fixture = Fixture::new();
    fixture.seed(
        "ws-a",
        "Team A",
        &[
            ("user-a", Role::Owner),
            ("user-b", Role::Admin),
            ("user-c", Role::Member),
        ],
    );
    fixture
}

fn denied_by<T: std::fmt::Debug>(result: Result<T, OrgError>) -> Check {
    match result {
        Err(OrgError::Denied(denial)) => denial.check,
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn a_scoped_key_creates_a_workspace_it_owns_but_stays_bound_to_its_own() {
    let fixture = team();
    let caller = key(
        "svc-a",
        "ws-a",
        &[Scope::WorkspaceWrite, Scope::WorkspaceRead],
    );
    let created = fixture
        .store
        .create_workspace(&caller, "  Team B, Ltd.  ")
        .unwrap();
    assert_eq!(
        (created.name.as_str(), created.handle.as_str(), created.role),
        ("Team B, Ltd.", "team-b-ltd", Some(Role::Owner))
    );
    assert_eq!(
        fixture.audit_actions(created.id.as_str()),
        ["workspace.create"]
    );
    // The creating credential stays bound to its own workspace.
    assert_eq!(
        denied_by(fixture.store.get_workspace(&caller, &created.id)),
        Check::WorkspaceBinding
    );
}

#[test]
fn creation_refuses_login_grants_and_blank_names() {
    let fixture = team();
    let grant = fixture
        .store
        .create_workspace(&login("user-a", "ws-a", WRITE), "Team B");
    assert_eq!(failure(grant), ("forbidden", Some("grant_workspace_bound")));
    let blank = fixture
        .store
        .create_workspace(&key("svc-a", "ws-a", WRITE), "   ");
    assert_eq!(failure(blank), ("invalid_request", None));
    let unscoped = fixture
        .store
        .create_workspace(&key("svc-a", "ws-a", READ), "Team B");
    assert_eq!(denied_by(unscoped), Check::Scope);
    assert!(fixture.audit_actions("ws-a").is_empty());
}

#[test]
fn get_and_list_report_the_bound_workspace_and_caller_role() {
    let fixture = team();
    fixture.seed("ws-b", "Team B", &[("user-c", Role::Owner)]);
    let member = login("user-c", "ws-a", READ);
    let found = fixture.store.get_workspace(&member, &ws("ws-a")).unwrap();
    assert_eq!(
        (found.name.as_str(), found.role),
        ("Team A", Some(Role::Member))
    );
    let listed = fixture.store.list_workspaces(&member).unwrap();
    assert_eq!(listed, [found]);
    assert_eq!(
        denied_by(fixture.store.get_workspace(&member, &ws("ws-b"))),
        Check::WorkspaceBinding
    );
}

#[test]
fn admin_renames_and_the_handle_is_derived_again() {
    let fixture = team();
    let admin = login("user-b", "ws-a", WRITE);
    let renamed = fixture
        .store
        .rename_workspace(&admin, &ws("ws-a"), "Team Alpha")
        .unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.handle.as_str()),
        ("Team Alpha", "team-alpha")
    );
    let again = fixture
        .store
        .rename_workspace(&admin, &ws("ws-a"), "Team Alpha")
        .unwrap();
    assert_eq!(again, renamed);
    assert_eq!(fixture.audit_actions("ws-a"), ["workspace.rename"]);
    let member =
        fixture
            .store
            .rename_workspace(&login("user-c", "ws-a", WRITE), &ws("ws-a"), "Mine");
    assert_eq!(denied_by(member), Check::Role);
    let blank = fixture.store.rename_workspace(&admin, &ws("ws-a"), " ");
    assert_eq!(failure(blank), ("invalid_request", None));
}

#[test]
fn only_the_owner_deletes_and_only_with_the_exact_name() {
    let fixture = team();
    let admin =
        fixture
            .store
            .delete_workspace(&login("user-b", "ws-a", WRITE), &ws("ws-a"), "Team A");
    assert_eq!(denied_by(admin), Check::Role);
    let owner = login("user-a", "ws-a", WRITE);
    let mismatch = fixture
        .store
        .delete_workspace(&owner, &ws("ws-a"), "team a");
    assert_eq!(
        failure(mismatch),
        ("invalid_request", Some("name_mismatch"))
    );
    assert!(fixture.audit_actions("ws-a").is_empty());
}

#[test]
fn deletion_revokes_every_bound_credential_in_the_same_commit() {
    let fixture = team();
    let credentials = CredentialStore::open(&fixture.path, fixture.clock.clone()).unwrap();
    let issued_key = credentials
        .issue_key(&who("svc-a"), &ws("ws-a"), [Scope::WorkspaceRead])
        .unwrap();
    let issued_login = credentials
        .issue_login(&who("user-c"), &ws("ws-a"), [], Duration::from_secs(600))
        .unwrap();
    let owner = login("user-a", "ws-a", WRITE);
    fixture
        .store
        .delete_workspace(&owner, &ws("ws-a"), "Team A")
        .unwrap();
    for secret in [issued_key.secret.expose(), issued_login.secret.expose()] {
        assert_eq!(
            credentials.authenticate(secret),
            Err(StoreError::Unauthenticated)
        );
    }
    assert_eq!(fixture.audit_actions("ws-a"), ["workspace.delete"]);
    // Every later request against the deleted workspace fails authentication.
    let after = fixture
        .store
        .get_workspace(&login("user-a", "ws-a", READ), &ws("ws-a"));
    assert_eq!(denied_by(after), Check::Authentication);
}
