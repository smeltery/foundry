//! Membership operations: invitations, reads, removal, role changes and the
//! owner invariants, each decided by the authorization core.
mod org_fixture;

use std::time::Duration;

use foundry_identity::authz::{Check, Role, Scope};
use foundry_identity::org::OrgError;
use foundry_identity::store::{CredentialStore, StoreError};
use org_fixture::{Fixture, failure, login, who, ws};

const READ: &[Scope] = &[Scope::WorkspaceRead];
const WRITE: &[Scope] = &[Scope::WorkspaceWrite];

/// ws-a: owner user-a, admin user-b, member user-c.
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

fn role_of(fixture: &Fixture, user: &str) -> Role {
    let owner = login("user-a", "ws-a", READ);
    fixture
        .store
        .get_member(&owner, &ws("ws-a"), &who(user))
        .unwrap()
        .role
}

#[test]
fn admin_list_includes_invitations_and_member_list_hides_them() {
    let fixture = team();
    let invited = fixture
        .store
        .invite_members(
            &login("user-b", "ws-a", WRITE),
            &ws("ws-a"),
            &["new@team.example"],
            None,
        )
        .unwrap();
    assert_eq!(
        (invited.role, invited.invited, invited.delivered),
        (Role::Admin, 1, 0)
    );
    let admin = fixture
        .store
        .list_members(&login("user-b", "ws-a", READ), &ws("ws-a"))
        .unwrap();
    let users: Vec<_> = admin
        .members
        .iter()
        .map(|member| member.user.as_str())
        .collect();
    assert_eq!(users, ["user-a", "user-b", "user-c"]);
    assert_eq!(admin.invitations.len(), 1);
    assert_eq!(admin.invitations[0].email, "new@team.example");
    let member = fixture
        .store
        .list_members(&login("user-c", "ws-a", READ), &ws("ws-a"))
        .unwrap();
    assert_eq!((member.members.len(), member.invitations.len()), (3, 0));
}

#[test]
fn invitations_validate_role_and_addresses() {
    let fixture = team();
    let owner = login("user-a", "ws-a", WRITE);
    let invite = |emails: &[&str], role| {
        fixture
            .store
            .invite_members(&owner, &ws("ws-a"), emails, role)
    };
    assert_eq!(
        failure(invite(&["one@team.example"], Some(Role::Owner))),
        ("invalid_request", None)
    );
    assert_eq!(failure(invite(&[], None)), ("invalid_request", None));
    for bad in [
        "",
        "plain",
        "@team.example",
        "a b@team.example",
        "a@b@team.example",
    ] {
        assert_eq!(
            failure(invite(&["ok@team.example", bad], None)),
            ("invalid_request", None)
        );
    }
    // A refused request records nothing, not even its valid addresses.
    assert!(fixture.audit_actions("ws-a").is_empty());
    let both = invite(
        &["one@team.example", "two@team.example"],
        Some(Role::Member),
    )
    .unwrap();
    assert_eq!((both.role, both.invited), (Role::Member, 2));
    assert_eq!(
        fixture.audit_actions("ws-a"),
        ["member.invite", "member.invite"]
    );
}

#[test]
fn member_reads_but_cannot_change_membership() {
    let fixture = team();
    let member = login(
        "user-c",
        "ws-a",
        &[Scope::WorkspaceRead, Scope::WorkspaceWrite],
    );
    let found = fixture
        .store
        .get_member(&member, &ws("ws-a"), &who("user-b"))
        .unwrap();
    assert_eq!(
        (found.role, found.email.as_deref()),
        (Role::Admin, Some("user-b@team.example"))
    );
    let denials = [
        fixture
            .store
            .invite_members(&member, &ws("ws-a"), &["x@team.example"], None)
            .map(|_| ()),
        fixture
            .store
            .remove_member(&member, &ws("ws-a"), &who("user-b")),
        fixture
            .store
            .set_member_role(&member, &ws("ws-a"), &who("user-c"), Role::Admin)
            .map(|_| ()),
    ];
    for denial in denials {
        let Err(OrgError::Denied(denial)) = denial else {
            panic!("{denial:?}")
        };
        assert_eq!(
            (denial.class.name(), denial.check),
            ("forbidden", Check::Role)
        );
    }
    assert!(fixture.audit_actions("ws-a").is_empty());
}

#[test]
fn missing_scope_and_foreign_workspace_are_refused_before_any_rule() {
    let fixture = team();
    fixture.seed("ws-b", "Team B", &[("user-z", Role::Owner)]);
    let read_only = login("user-a", "ws-a", READ);
    let Err(OrgError::Denied(denial)) =
        fixture
            .store
            .remove_member(&read_only, &ws("ws-a"), &who("user-c"))
    else {
        panic!("removal without workspace.write must be denied")
    };
    assert_eq!(denial.check, Check::Scope);
    let owner = login("user-a", "ws-a", READ);
    let Err(OrgError::Denied(denial)) =
        fixture
            .store
            .get_member(&owner, &ws("ws-b"), &who("user-z"))
    else {
        panic!("another workspace must not be visible")
    };
    assert_eq!(
        (denial.class.name(), denial.check),
        ("not_found", Check::WorkspaceBinding)
    );
}

#[test]
fn non_member_reads_are_not_member() {
    let fixture = team();
    let result =
        fixture
            .store
            .get_member(&login("user-c", "ws-a", READ), &ws("ws-a"), &who("user-z"));
    assert_eq!(failure(result), ("not_found", Some("not_member")));
}

#[test]
fn admin_removes_member_and_revokes_only_that_login() {
    let fixture = team();
    let credentials = CredentialStore::open(&fixture.path, fixture.clock.clone()).unwrap();
    let lifetime = Duration::from_secs(600);
    let removed = credentials
        .issue_login(&who("user-c"), &ws("ws-a"), [], lifetime)
        .unwrap();
    let kept = credentials
        .issue_login(&who("user-b"), &ws("ws-a"), [], lifetime)
        .unwrap();
    let admin = login("user-b", "ws-a", WRITE);
    fixture
        .store
        .remove_member(&admin, &ws("ws-a"), &who("user-c"))
        .unwrap();
    assert_eq!(
        credentials.authenticate(removed.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    assert!(credentials.authenticate(kept.secret.expose()).is_ok());
    // The removed user's next request no longer authenticates.
    let Err(OrgError::Denied(denial)) = fixture
        .store
        .list_members(&login("user-c", "ws-a", READ), &ws("ws-a"))
    else {
        panic!("a removed member must be refused")
    };
    assert_eq!(denial.check, Check::Authentication);
    let again = fixture
        .store
        .remove_member(&admin, &ws("ws-a"), &who("user-c"));
    assert_eq!(failure(again), ("not_found", Some("not_member")));
    assert_eq!(fixture.audit_actions("ws-a"), ["member.remove"]);
}

#[test]
fn only_an_owner_touches_owner_memberships() {
    let fixture = team();
    let admin = login("user-b", "ws-a", WRITE);
    let remove = fixture
        .store
        .remove_member(&admin, &ws("ws-a"), &who("user-a"));
    assert_eq!(failure(remove), ("forbidden", Some("owner_required")));
    let promote = fixture
        .store
        .set_member_role(&admin, &ws("ws-a"), &who("user-c"), Role::Owner);
    assert_eq!(failure(promote), ("forbidden", Some("owner_required")));
    let demote = fixture
        .store
        .set_member_role(&admin, &ws("ws-a"), &who("user-a"), Role::Member);
    assert_eq!(failure(demote), ("forbidden", Some("owner_required")));
    let owner = login("user-a", "ws-a", WRITE);
    let promoted = fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-c"), Role::Owner)
        .unwrap();
    assert_eq!(promoted.role, Role::Owner);
    assert_eq!(fixture.audit_actions("ws-a"), ["member.set_role"]);
}

#[test]
fn the_last_owner_can_be_neither_removed_nor_demoted() {
    let fixture = team();
    let owner = login("user-a", "ws-a", WRITE);
    let remove = fixture
        .store
        .remove_member(&owner, &ws("ws-a"), &who("user-a"));
    assert_eq!(failure(remove), ("conflict", Some("last_owner")));
    let demote = fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-a"), Role::Admin);
    assert_eq!(failure(demote), ("conflict", Some("last_owner")));
    assert_eq!(role_of(&fixture, "user-a"), Role::Owner);
    // With a second owner, the first may step down.
    fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-b"), Role::Owner)
        .unwrap();
    fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-a"), Role::Admin)
        .unwrap();
    assert_eq!(role_of(&fixture, "user-b"), Role::Owner);
    let new_owner = login("user-b", "ws-a", WRITE);
    let remove = fixture
        .store
        .remove_member(&new_owner, &ws("ws-a"), &who("user-b"));
    assert_eq!(failure(remove), ("conflict", Some("last_owner")));
}

#[test]
fn repeating_a_role_reports_the_same_state_and_viewer_is_not_assignable() {
    let fixture = team();
    let admin = login("user-b", "ws-a", WRITE);
    let same = fixture
        .store
        .set_member_role(&admin, &ws("ws-a"), &who("user-c"), Role::Member)
        .unwrap();
    assert_eq!(same.role, Role::Member);
    let viewer = fixture
        .store
        .set_member_role(&admin, &ws("ws-a"), &who("user-c"), Role::Viewer);
    assert_eq!(failure(viewer), ("invalid_request", None));
    assert!(fixture.audit_actions("ws-a").is_empty());
}
