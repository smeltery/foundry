//! The rules behind `single_use.create`, `settings.list` and `settings.get`.
//! The decision core denies those rows for every caller until their matrix
//! cells are established, so these tests run the handlers directly inside a
//! transaction, as if authorization had allowed them.
use rusqlite::Connection;

use super::settings::{SettingsCatalog, SettingsNamespace};
use super::single_use::{LIFETIME_DEFAULT_MINUTES, SingleUseRequest};
use super::{Authorized, OrgError};
use crate::authz::{Access, Credential, CredentialKind, Principal, PrincipalId, WorkspaceId};
use crate::store::database;

fn connection() -> Connection {
    let connection = database::open_in_memory().unwrap();
    connection
        .execute_batch(
            "INSERT INTO org_workspaces (id, name, handle, state, created_ms)
             VALUES ('ws-a', 'Team A', 'team-a', 'active', 0);
             INSERT INTO org_members (workspace, principal, role, joined_ms)
             VALUES ('ws-a', 'user-a', 'owner', 0);
             INSERT INTO org_settings (workspace, namespace, value) VALUES
                 ('ws-a', 'defaults', '{\"model\":\"fixture\"}'),
                 ('ws-a', 'sensitive', '{\"x\":1}'),
                 ('ws-a', 'retired', '{}');",
        )
        .unwrap();
    connection
}

fn caller() -> Principal {
    Principal::new(
        PrincipalId::new("user-a"),
        Credential::new(CredentialKind::LoginGrant, WorkspaceId::new("ws-a")),
    )
}

fn parent() -> WorkspaceId {
    WorkspaceId::new("ws-a")
}

fn request(trial_key: &str, lifetime_minutes: Option<u32>) -> SingleUseRequest {
    SingleUseRequest {
        trial_key: trial_key.to_owned(),
        lifetime_minutes,
    }
}

fn outcome(error: OrgError) -> (&'static str, Option<&'static str>) {
    (error.class(), error.cause())
}

#[test]
fn single_use_limits_follow_the_specification() {
    let mut connection = connection();
    let transaction = connection.transaction().unwrap();
    let caller = caller();
    let authorized = Authorized {
        transaction: &transaction,
        now_ms: 0,
        caller: &caller,
        access: Access::Full,
    };
    let longest = "k".repeat(200);
    let accepted = [
        ("a", None, LIFETIME_DEFAULT_MINUTES),
        ("x.y_z:1-2", Some(5), 5),
        (longest.as_str(), Some(720), 720),
    ];
    for (key, lifetime, expected) in accepted {
        let created = authorized
            .create_single_use(&parent(), &request(key, lifetime))
            .unwrap();
        assert_eq!(created.record.lifetime_minutes, expected);
        assert_eq!(created.record.expires_ms, u64::from(expected) * 60_000);
    }
    let longer = "k".repeat(201);
    let refused = [
        ("", None),
        (longer.as_str(), None),
        ("trial 1", None),
        ("trial/1", None),
        ("t4", Some(4)),
        ("t721", Some(721)),
    ];
    for (key, lifetime) in refused {
        let error = authorized
            .create_single_use(&parent(), &request(key, lifetime))
            .unwrap_err();
        assert_eq!(
            outcome(error),
            ("invalid_request", None),
            "{key:?} {lifetime:?}"
        );
    }
}

#[test]
fn single_use_replays_conflicts_and_ownership() {
    let mut connection = connection();
    let transaction = connection.transaction().unwrap();
    let caller = caller();
    let at = |now_ms| Authorized {
        transaction: &transaction,
        now_ms,
        caller: &caller,
        access: Access::Full,
    };
    let first = at(0)
        .create_single_use(&parent(), &request("trial-1", None))
        .unwrap();
    assert!(!first.replayed);
    let replay = at(1)
        .create_single_use(&parent(), &request("trial-1", Some(240)))
        .unwrap();
    assert_eq!((replay.replayed, &replay.record), (true, &first.record));
    let reused = at(1)
        .create_single_use(&parent(), &request("trial-1", Some(60)))
        .unwrap_err();
    assert_eq!(outcome(reused), ("conflict", Some("key_reused")));
    let expired = at(first.record.expires_ms)
        .create_single_use(&parent(), &request("trial-1", None))
        .unwrap_err();
    assert_eq!(outcome(expired), ("conflict", Some("workspace_retired")));
    let (owner, audited): (String, i64) = transaction
        .query_row(
            "SELECT (SELECT role FROM org_members WHERE workspace = ?1 AND principal = 'user-a'),
                    (SELECT count(*) FROM org_audit WHERE workspace = 'ws-a'
                       AND action = 'single_use.create' AND target = ?1)",
            [first.record.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((owner.as_str(), audited), ("owner", 1));
}

#[test]
fn settings_reads_omit_unset_sensitive_and_unknown_namespaces() {
    let mut connection = connection();
    let transaction = connection.transaction().unwrap();
    let caller = caller();
    let authorized = Authorized {
        transaction: &transaction,
        now_ms: 0,
        caller: &caller,
        access: Access::Full,
    };
    let namespace = |name, sensitive| SettingsNamespace {
        name,
        sensitive,
        writable: true,
        validate: |_| true,
    };
    let catalog = SettingsCatalog::new([
        namespace("defaults", false),
        namespace("sensitive", true),
        namespace("unset", false),
    ]);
    let listed = authorized.settings(&parent(), &catalog).unwrap();
    let names: Vec<_> = listed.keys().map(String::as_str).collect();
    assert_eq!(names, ["defaults"]);
    assert_eq!(
        authorized
            .setting(&parent(), &catalog, "defaults")
            .unwrap()
            .as_deref(),
        Some("{\"model\":\"fixture\"}")
    );
    assert_eq!(
        authorized.setting(&parent(), &catalog, "unset").unwrap(),
        None
    );
    for hidden in ["sensitive", "retired"] {
        let error = authorized.setting(&parent(), &catalog, hidden).unwrap_err();
        assert_eq!(outcome(error), ("not_found", None));
    }
}
