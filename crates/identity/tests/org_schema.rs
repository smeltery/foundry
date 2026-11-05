//! The organization schema step, one-transaction mutations, and concurrent
//! role changes from separate connections.
mod org_fixture;

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use foundry_identity::authz::{Role, Scope};
use foundry_identity::org::{OrgError, OrgStore};
use foundry_identity::store::{CredentialStore, IDENTITY_DATABASE, ManualClock};
use org_fixture::{Fixture, failure, login, who, ws};
use rusqlite::Connection;

const WRITE: &[Scope] = &[Scope::WorkspaceWrite];

/// The identity schema as version 1 shipped it, frozen here so the step is
/// exercised against the real previous layout.
const VERSION_1: &str = "
CREATE TABLE identity_schema_version (version INTEGER NOT NULL);
CREATE TABLE credentials (
    sequence INTEGER PRIMARY KEY,
    prefix TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    scopes TEXT NOT NULL,
    secret_digest BLOB NOT NULL,
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER,
    revoked_ms INTEGER
);
CREATE INDEX credentials_by_holder ON credentials (workspace, kind, principal);
CREATE TABLE pairings (
    prefix TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    secret_digest BLOB,
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    max_attempts INTEGER NOT NULL,
    failed_attempts INTEGER NOT NULL,
    client TEXT,
    claimed_ms INTEGER
);
INSERT INTO identity_schema_version (version) VALUES (1);
INSERT INTO credentials (prefix, kind, principal, workspace, scopes, secret_digest, created_ms)
VALUES ('key_KEPTKEPTKE', 'scoped_key', 'svc-a', 'ws-a', 'workspace.read', x'00', 0);
PRAGMA application_id = 1179206734;
";

fn version_1_file(directory: &Path) -> std::path::PathBuf {
    let path = directory.join(IDENTITY_DATABASE);
    Connection::open(&path)
        .unwrap()
        .execute_batch(VERSION_1)
        .unwrap();
    path
}

fn scalar(path: &Path, query: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(query, [], |row| row.get(0))
        .unwrap()
}

fn open(path: &Path) -> Result<OrgStore, OrgError> {
    OrgStore::open(path, Arc::new(ManualClock::default()))
}

#[test]
fn a_version_1_file_gains_the_organization_tables_and_keeps_its_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = version_1_file(directory.path());
    drop(open(&path).unwrap());
    assert_eq!(
        scalar(&path, "SELECT version FROM identity_schema_version"),
        2
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM credentials WHERE prefix = 'key_KEPTKEPTKE'"
        ),
        1
    );
    let tables = scalar(
        &path,
        "SELECT count(*) FROM sqlite_master WHERE type IN ('table', 'trigger')
         AND name LIKE 'org_%'",
    );
    assert_eq!(tables, 7);
    // Both stores open the migrated file, and the step does not run twice.
    CredentialStore::open(&path, Arc::new(ManualClock::default())).unwrap();
    drop(open(&path).unwrap());
    assert_eq!(
        scalar(&path, "SELECT version FROM identity_schema_version"),
        2
    );
}

#[test]
fn a_failed_step_leaves_the_version_1_file_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = version_1_file(directory.path());
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE org_audit (body TEXT);")
        .unwrap();
    assert_eq!(open(&path).unwrap_err().class(), "unavailable");
    assert_eq!(
        scalar(&path, "SELECT version FROM identity_schema_version"),
        1
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM sqlite_master WHERE name = 'org_workspaces'"
        ),
        0
    );
}

#[test]
fn newer_and_foreign_files_are_still_refused() {
    let directory = tempfile::tempdir().unwrap();
    let newer = directory.path().join("newer.sqlite3");
    drop(open(&newer).unwrap());
    Connection::open(&newer)
        .unwrap()
        .execute_batch("UPDATE identity_schema_version SET version = 3")
        .unwrap();
    assert_eq!(open(&newer).unwrap_err().class(), "unavailable");
    let foreign = directory.path().join("foreign.sqlite3");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE notes (body TEXT);")
        .unwrap();
    assert_eq!(open(&foreign).unwrap_err().class(), "unavailable");
}

#[test]
fn a_mutation_whose_audit_entry_fails_is_rolled_back() {
    let fixture = Fixture::new();
    fixture.seed(
        "ws-a",
        "Team A",
        &[("user-a", Role::Owner), ("user-c", Role::Member)],
    );
    fixture
        .raw()
        .execute_batch(
            "CREATE TRIGGER refuse_audit BEFORE INSERT ON org_audit
             BEGIN SELECT RAISE(ABORT, 'audit sink down'); END;",
        )
        .unwrap();
    let owner = login("user-a", "ws-a", WRITE);
    let changed = fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-c"), Role::Admin);
    assert_eq!(failure(changed), ("unavailable", None));
    let renamed = fixture
        .store
        .rename_workspace(&owner, &ws("ws-a"), "Renamed");
    assert_eq!(failure(renamed), ("unavailable", None));
    let raw = fixture.raw();
    let (role, name): (String, String) = raw
        .query_row(
            "SELECT m.role, w.name FROM org_members m JOIN org_workspaces w ON w.id = m.workspace
             WHERE m.principal = 'user-c'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((role.as_str(), name.as_str()), ("member", "Team A"));
}

/// Runs both changes at once, each on its own connection, and returns their
/// results with the number of owners left.
fn race(first: (&str, &str, Role), second: (&str, &str, Role)) -> (Vec<Result<(), OrgError>>, i64) {
    let fixture = Fixture::new();
    fixture.seed(
        "ws-a",
        "Team A",
        &[("user-a", Role::Owner), ("user-b", Role::Owner)],
    );
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|(caller, target, role)| {
            let (store, barrier) = (fixture.connect(), barrier.clone());
            let (caller, target) = (login(caller, "ws-a", WRITE), who(target));
            thread::spawn(move || {
                barrier.wait();
                store
                    .set_member_role(&caller, &ws("ws-a"), &target, role)
                    .map(|_| ())
            })
        })
        .collect();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let owners = scalar(
        &fixture.path,
        "SELECT count(*) FROM org_members WHERE role = 'owner'",
    );
    (results, owners)
}

#[test]
fn concurrent_owners_demoting_each_other_leave_one_owner() {
    for _ in 0..10 {
        let (results, owners) = race(
            ("user-a", "user-b", Role::Admin),
            ("user-b", "user-a", Role::Admin),
        );
        assert_eq!(owners, 1);
        let refused: Vec<_> = results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| (error.class(), error.cause()))
            .collect();
        // The loser is an admin by the time it decides.
        assert_eq!(refused, [("forbidden", Some("owner_required"))]);
    }
}

#[test]
fn concurrent_owners_stepping_down_leave_one_owner() {
    for _ in 0..10 {
        let (results, owners) = race(
            ("user-a", "user-a", Role::Member),
            ("user-b", "user-b", Role::Member),
        );
        assert_eq!(owners, 1);
        let refused: Vec<_> = results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| (error.class(), error.cause()))
            .collect();
        assert_eq!(refused, [("conflict", Some("last_owner"))]);
    }
}
