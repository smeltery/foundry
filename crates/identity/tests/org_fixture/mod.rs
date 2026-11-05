//! Shared setup for the organization tests. Bootstrapping a workspace and
//! accepting invitations belong to the identity authority (decision C1), so
//! the given state is written with SQL, as committed state the operations read.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use foundry_identity::authz::{
    Credential, CredentialKind, Principal, PrincipalId, Role, Scope, WorkspaceId,
};
use foundry_identity::org::{OrgError, OrgStore};
use foundry_identity::store::{IDENTITY_DATABASE, ManualClock};
use rusqlite::{Connection, params};

pub fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

pub fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

/// A user's login grant bound to `workspace`.
pub fn login(user: &str, workspace: &str, scopes: &[Scope]) -> Principal {
    Principal::new(
        who(user),
        Credential::new(CredentialKind::LoginGrant, ws(workspace)).with_scopes(scopes.to_vec()),
    )
}

/// A service principal's scoped key bound to `workspace`.
pub fn key(service: &str, workspace: &str, scopes: &[Scope]) -> Principal {
    Principal::new(
        who(service),
        Credential::new(CredentialKind::ScopedKey, ws(workspace)).with_scopes(scopes.to_vec()),
    )
}

/// The class and cause an error reports, for compact assertions.
pub fn failure<T: std::fmt::Debug>(
    result: Result<T, OrgError>,
) -> (&'static str, Option<&'static str>) {
    let error = result.unwrap_err();
    (error.class(), error.cause())
}

pub struct Fixture {
    _directory: tempfile::TempDir,
    pub path: PathBuf,
    pub clock: Arc<ManualClock>,
    pub store: OrgStore,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(IDENTITY_DATABASE);
        let clock = Arc::new(ManualClock::default());
        let store = OrgStore::open(&path, clock.clone()).unwrap();
        Self {
            _directory: directory,
            path,
            clock,
            store,
        }
    }

    /// Another store on its own connection to the same file.
    pub fn connect(&self) -> OrgStore {
        OrgStore::open(&self.path, self.clock.clone()).unwrap()
    }

    pub fn raw(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }

    /// An active workspace named `name` with these members.
    pub fn seed(&self, workspace: &str, name: &str, members: &[(&str, Role)]) {
        let raw = self.raw();
        raw.execute(
            "INSERT INTO org_workspaces (id, name, handle, state, created_ms)
             VALUES (?1, ?2, ?2, 'active', 0)",
            params![workspace, name],
        )
        .unwrap();
        for (user, role) in members {
            raw.execute(
                "INSERT INTO org_members (workspace, principal, role, email, joined_ms)
                 VALUES (?1, ?2, ?3, ?2 || '@team.example', 0)",
                params![workspace, user, role.name()],
            )
            .unwrap();
        }
    }

    /// A live single-use workspace of `parent` that expires at `expires_ms`.
    pub fn seed_single_use(&self, id: &str, parent: &str, expires_ms: u64) {
        self.raw()
            .execute(
                "INSERT INTO org_workspaces (id, name, handle, state, created_ms, parent,
                     trial_key, lifetime_minutes, expires_ms)
                 VALUES (?1, ?1, ?1, 'active', 0, ?2, ?1, 240, ?3)",
                params![id, parent, i64::try_from(expires_ms).unwrap()],
            )
            .unwrap();
    }

    /// Every audit action recorded for `workspace`, oldest first.
    pub fn audit_actions(&self, workspace: &str) -> Vec<String> {
        let raw = self.raw();
        let mut statement = raw
            .prepare("SELECT action FROM org_audit WHERE workspace = ?1 ORDER BY sequence")
            .unwrap();
        statement
            .query_map([workspace], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}
