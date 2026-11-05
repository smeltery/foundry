//! Settings namespaces and single-use workspaces through the decision core,
//! including the rows whose matrix cells the specification leaves unknown.
mod org_fixture;

use foundry_identity::authz::{Check, Role, Scope};
use foundry_identity::org::{
    OrgError, RetireCause, SettingsCatalog, SettingsNamespace, SingleUseRequest,
};
use foundry_identity::store::StoreError;
use org_fixture::{Fixture, failure, who, ws};

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
            ("user-d", Role::Viewer),
        ],
    );
    fixture
}

fn object(value: &str) -> bool {
    value.starts_with('{') && value.ends_with('}')
}

fn catalog() -> SettingsCatalog {
    SettingsCatalog::new([
        SettingsNamespace {
            name: "defaults",
            sensitive: false,
            writable: true,
            validate: object,
        },
        SettingsNamespace {
            name: "fixed",
            sensitive: false,
            writable: false,
            validate: object,
        },
    ])
}

fn denied_by<T: std::fmt::Debug>(result: Result<T, OrgError>) -> Check {
    match result {
        Err(OrgError::Denied(denial)) => denial.check,
        other => panic!("expected a denial, got {other:?}"),
    }
}

fn stored_setting(fixture: &Fixture) -> Option<String> {
    fixture
        .raw()
        .query_row(
            "SELECT value FROM org_settings WHERE workspace = 'ws-a' AND namespace = 'defaults'",
            [],
            |row| row.get(0),
        )
        .ok()
}

#[test]
fn admin_replaces_a_whole_value_without_auditing_it() {
    let fixture = team();
    let admin = fixture.login("user-b", "ws-a", WRITE);
    let replace = |value| {
        fixture
            .store
            .replace_setting(&admin, &ws("ws-a"), &catalog(), "defaults", value)
    };
    replace(r#"{"model":"fixture","effort":"low"}"#).unwrap();
    replace(r#"{"model":"other"}"#).unwrap();
    replace(r#"{"model":"other"}"#).unwrap();
    assert_eq!(
        stored_setting(&fixture).as_deref(),
        Some(r#"{"model":"other"}"#)
    );
    assert_eq!(
        fixture.audit_actions("ws-a"),
        ["settings.replace", "settings.replace"]
    );
    let details: Vec<String> = fixture
        .raw()
        .prepare("SELECT target || details FROM org_audit")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(details.iter().all(|text| text == "defaults"));
}

#[test]
fn replace_refuses_unknown_fixed_and_invalid_values() {
    let fixture = team();
    let owner = fixture.login("user-a", "ws-a", WRITE);
    for (namespace, value) in [("unlisted", "{}"), ("fixed", "{}"), ("defaults", "[]")] {
        let result =
            fixture
                .store
                .replace_setting(&owner, &ws("ws-a"), &catalog(), namespace, value);
        assert_eq!(failure(result), ("invalid_request", None));
    }
    let member = fixture.store.replace_setting(
        &fixture.login("user-c", "ws-a", WRITE),
        &ws("ws-a"),
        &catalog(),
        "defaults",
        "{}",
    );
    assert_eq!(denied_by(member), Check::Role);
    assert_eq!(stored_setting(&fixture), None);
}

#[test]
fn settings_reads_stay_denied_while_their_matrix_cells_are_unknown() {
    let fixture = team();
    for user in ["user-a", "user-b", "user-c", "user-d"] {
        let caller = fixture.login(user, "ws-a", READ);
        let list = fixture
            .store
            .list_settings(&caller, &ws("ws-a"), &catalog());
        assert_eq!(denied_by(list), Check::UndeterminedRole);
        let get = fixture
            .store
            .get_setting(&caller, &ws("ws-a"), &catalog(), "defaults");
        assert_eq!(denied_by(get), Check::UndeterminedRole);
    }
}

#[test]
fn single_use_creation_stays_denied_while_its_matrix_cell_is_unknown() {
    let fixture = team();
    let request = SingleUseRequest {
        trial_key: "trial-1".to_owned(),
        lifetime_minutes: None,
    };
    let callers = ["user-a", "user-b", "user-c", "user-d"]
        .map(|user| fixture.login(user, "ws-a", WRITE))
        .into_iter()
        .chain([fixture.key("svc-a", "ws-a", WRITE)]);
    for caller in callers {
        let created = fixture
            .store
            .create_single_use(&caller, &ws("ws-a"), &request);
        assert_eq!(denied_by(created), Check::UndeterminedRole);
    }
    let count: i64 = fixture
        .raw()
        .query_row("SELECT count(*) FROM org_workspaces", [], |row| row.get(0))
        .unwrap();
    assert_eq!((count, fixture.audit_actions("ws-a").len()), (1, 0));
}

#[test]
fn admin_retires_and_a_repeat_returns_the_saved_result() {
    let fixture = team();
    fixture.seed_single_use("su-1", "ws-a", 60_000);
    let token = fixture
        .credentials
        .issue_key(&who("svc-su"), &ws("su-1"), [Scope::WorkspaceRead])
        .unwrap();
    let inside = fixture.caller(&token);
    let admin = fixture.login("user-b", "ws-a", WRITE);
    let retired = fixture
        .store
        .retire_single_use(&admin, &ws("ws-a"), &ws("su-1"))
        .unwrap();
    assert_eq!(retired.cause, RetireCause::Requested);
    assert_eq!((retired.cancelled_runs, retired.revoked_machines), (0, 0));
    assert_eq!(retired.record.retired_ms, Some(0));
    assert_eq!(
        fixture.credentials.authenticate(token.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    // A caller that authenticated inside the workspace before it retired is
    // refused on its next operation.
    assert_eq!(
        denied_by(fixture.store.list_workspaces(&inside)),
        Check::Authentication
    );
    fixture.clock.advance(120_000);
    let again = fixture
        .store
        .retire_single_use(
            &fixture.login("user-a", "ws-a", WRITE),
            &ws("ws-a"),
            &ws("su-1"),
        )
        .unwrap();
    assert_eq!(again, retired);
    assert_eq!(fixture.audit_actions("ws-a"), ["single_use.retire"]);
}

#[test]
fn retiring_after_expiry_saves_the_expired_cause() {
    let fixture = team();
    fixture.seed_single_use("su-1", "ws-a", 1_000);
    fixture.clock.advance(1_000);
    let outcome = fixture
        .store
        .retire_single_use(
            &fixture.login("user-a", "ws-a", WRITE),
            &ws("ws-a"),
            &ws("su-1"),
        )
        .unwrap();
    assert_eq!(outcome.cause, RetireCause::Expired);
}

#[test]
fn retirement_is_refused_for_members_and_other_parents() {
    let fixture = team();
    fixture.seed("ws-b", "Team B", &[("user-z", Role::Owner)]);
    fixture.seed_single_use("su-1", "ws-a", 60_000);
    fixture.seed_single_use("su-9", "ws-b", 60_000);
    let member = fixture.store.retire_single_use(
        &fixture.login("user-c", "ws-a", WRITE),
        &ws("ws-a"),
        &ws("su-1"),
    );
    assert_eq!(denied_by(member), Check::Role);
    let owner = fixture.login("user-a", "ws-a", WRITE);
    for (parent, single_use) in [("ws-a", "su-9"), ("ws-a", "su-unknown"), ("ws-b", "su-9")] {
        let result = fixture
            .store
            .retire_single_use(&owner, &ws(parent), &ws(single_use));
        assert_eq!(failure(result), ("not_found", None));
    }
    assert!(fixture.audit_actions("ws-a").is_empty());
}

#[test]
fn deleting_the_parent_retires_its_live_single_use_workspaces() {
    let fixture = team();
    fixture.seed_single_use("su-live", "ws-a", 60_000);
    fixture.seed_single_use("su-done", "ws-a", 60_000);
    let owner = fixture.login("user-a", "ws-a", WRITE);
    fixture
        .store
        .retire_single_use(&owner, &ws("ws-a"), &ws("su-done"))
        .unwrap();
    let token = fixture
        .credentials
        .issue_key(&who("svc-su"), &ws("su-live"), [Scope::WorkspaceRead])
        .unwrap();
    fixture.clock.advance(5);
    fixture
        .store
        .delete_workspace(&owner, &ws("ws-a"), "Team A")
        .unwrap();
    assert_eq!(
        fixture.credentials.authenticate(token.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    let (retired_ms, cause): (i64, String) = fixture
        .raw()
        .query_row(
            "SELECT retired_ms, retire_cause FROM org_workspaces WHERE id = 'su-live'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((retired_ms, cause.as_str()), (5, "requested"));
    assert_eq!(
        fixture.audit_actions("ws-a"),
        ["single_use.retire", "single_use.retire", "workspace.delete"]
    );
}
