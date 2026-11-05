//! A credential revoked, logged out or expired after the caller authenticated
//! refuses every mutation, because each transaction reads the credential row
//! again before deciding. Scopes come from the row, not from the caller.
mod org_fixture;

use foundry_identity::authz::{Check, Principal, Role, Scope};
use foundry_identity::org::{Caller, OrgError, OrgStore, SettingsCatalog, SettingsNamespace};
use org_fixture::{Fixture, LOGIN_LIFETIME, who, ws};

const WRITE: &[Scope] = &[Scope::WorkspaceWrite];

fn team() -> Fixture {
    let fixture = Fixture::new();
    fixture.seed(
        "ws-a",
        "Team A",
        &[
            ("user-a", Role::Owner),
            ("user-b", Role::Owner),
            ("user-c", Role::Member),
        ],
    );
    fixture.seed_single_use("su-1", "ws-a", u64::MAX / 4);
    fixture
}

fn catalog() -> SettingsCatalog {
    SettingsCatalog::new([SettingsNamespace {
        name: "defaults",
        sensitive: false,
        writable: true,
        validate: |_| true,
    }])
}

/// Every owner mutation, each of which would succeed for a live credential.
fn mutations(store: &OrgStore, caller: &Caller) -> Vec<(&'static str, Result<(), OrgError>)> {
    let workspace = ws("ws-a");
    vec![
        (
            "rename",
            store
                .rename_workspace(caller, &workspace, "Renamed")
                .map(|_| ()),
        ),
        (
            "invite",
            store
                .invite_members(caller, &workspace, &["new@team.example"], None)
                .map(|_| ()),
        ),
        (
            "set_role",
            store
                .set_member_role(caller, &workspace, &who("user-c"), Role::Admin)
                .map(|_| ()),
        ),
        (
            "remove",
            store.remove_member(caller, &workspace, &who("user-b")),
        ),
        (
            "settings",
            store.replace_setting(caller, &workspace, &catalog(), "defaults", "{}"),
        ),
        (
            "retire",
            store
                .retire_single_use(caller, &workspace, &ws("su-1"))
                .map(|_| ()),
        ),
        (
            "delete",
            store.delete_workspace(caller, &workspace, "Team A"),
        ),
    ]
}

fn assert_all_unauthenticated(fixture: &Fixture, caller: &Caller) {
    for (name, result) in mutations(&fixture.store, caller) {
        let Err(OrgError::Denied(denial)) = result else {
            panic!("{name} must be denied, got {result:?}")
        };
        assert_eq!(denial.check, Check::Authentication, "{name}");
    }
    assert!(fixture.audit_actions("ws-a").is_empty());
}

#[test]
fn a_login_logged_out_after_authentication_refuses_every_mutation() {
    let fixture = team();
    let (issued, caller) = fixture.issue_login("user-a", "ws-a", WRITE);
    assert!(fixture.credentials.logout(issued.secret.expose()).unwrap());
    assert_all_unauthenticated(&fixture, &caller);
}

#[test]
fn a_login_revoked_after_authentication_refuses_every_mutation() {
    let fixture = team();
    let caller = fixture.login("user-a", "ws-a", WRITE);
    fixture
        .credentials
        .revoke_logins(&who("user-a"), &ws("ws-a"))
        .unwrap();
    assert_all_unauthenticated(&fixture, &caller);
}

#[test]
fn a_login_that_expires_after_authentication_refuses_every_mutation() {
    let fixture = team();
    let caller = fixture.login("user-a", "ws-a", WRITE);
    let lifetime_ms = u64::try_from(LOGIN_LIFETIME.as_millis()).unwrap();
    fixture.clock.advance(lifetime_ms);
    assert_all_unauthenticated(&fixture, &caller);
}

#[test]
fn a_key_revoked_after_authentication_cannot_create_a_workspace() {
    let fixture = team();
    let issued = fixture
        .credentials
        .issue_key(&who("svc-a"), &ws("ws-a"), [Scope::WorkspaceWrite])
        .unwrap();
    let caller = fixture.caller(&issued);
    assert!(
        fixture
            .credentials
            .revoke_key(&ws("ws-a"), &issued.record.prefix)
            .unwrap()
    );
    let Err(OrgError::Denied(denial)) = fixture.store.create_workspace(&caller, "Team B") else {
        panic!("a revoked key must not create a workspace")
    };
    assert_eq!(denial.check, Check::Authentication);
}

#[test]
fn presented_scopes_and_mismatched_prefixes_grant_nothing() {
    let fixture = team();
    let (issued, read_only) = fixture.issue_login("user-a", "ws-a", &[Scope::WorkspaceRead]);
    // A caller claiming more scopes than its stored credential holds.
    let mut claimed = read_only.principal().clone();
    claimed.credential.scopes.insert(Scope::WorkspaceWrite);
    let inflated = Caller::new(claimed, issued.record.prefix.clone());
    let Err(OrgError::Denied(denial)) =
        fixture
            .store
            .rename_workspace(&inflated, &ws("ws-a"), "Renamed")
    else {
        panic!("stored scopes must replace presented ones")
    };
    assert_eq!(denial.check, Check::Scope);
    // Another user's principal presented with this credential's prefix.
    let other = fixture.login("user-b", "ws-a", WRITE);
    let borrowed = Caller::new(
        Principal::clone(other.principal()),
        issued.record.prefix.clone(),
    );
    let Err(OrgError::Denied(denial)) =
        fixture
            .store
            .rename_workspace(&borrowed, &ws("ws-a"), "Renamed")
    else {
        panic!("a prefix held by another principal must not authenticate")
    };
    assert_eq!(denial.check, Check::Authentication);
    assert!(fixture.audit_actions("ws-a").is_empty());
}
