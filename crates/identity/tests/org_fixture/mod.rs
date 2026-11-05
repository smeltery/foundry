//! Shared setup for the organization tests. Bootstrapping a workspace and
//! accepting invitations belong to the identity authority (decision C1), so
//! the given state is written with SQL, as committed state the operations read.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use foundry_identity::authz::{PrincipalId, Role, Scope, WorkspaceId};
use foundry_identity::org::{Caller, OrgError, OrgStore};
use foundry_identity::store::{CredentialStore, IDENTITY_DATABASE, IssuedCredential, ManualClock};
use rusqlite::{Connection, params};

pub fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

pub fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

/// How long fixture logins stay valid.
pub const LOGIN_LIFETIME: Duration = Duration::from_secs(86_400);

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
    pub credentials: CredentialStore,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(IDENTITY_DATABASE);
        let clock = Arc::new(ManualClock::default());
        let store = OrgStore::open(&path, clock.clone()).unwrap();
        let credentials = CredentialStore::open(&path, clock.clone()).unwrap();
        Self {
            _directory: directory,
            path,
            clock,
            store,
            credentials,
        }
    }

    /// The caller a presented credential authenticates as.
    pub fn caller(&self, issued: &IssuedCredential) -> Caller {
        let principal = self
            .credentials
            .authenticate(issued.secret.expose())
            .unwrap();
        Caller::new(principal, issued.record.prefix.clone())
    }

    /// A stored login for `user` bound to `workspace`, and its caller.
    pub fn issue_login(
        &self,
        user: &str,
        workspace: &str,
        scopes: &[Scope],
    ) -> (IssuedCredential, Caller) {
        let issued = self
            .credentials
            .issue_login(&who(user), &ws(workspace), scopes.to_vec(), LOGIN_LIFETIME)
            .unwrap();
        let caller = self.caller(&issued);
        (issued, caller)
    }

    /// A user's login grant bound to `workspace`.
    pub fn login(&self, user: &str, workspace: &str, scopes: &[Scope]) -> Caller {
        self.issue_login(user, workspace, scopes).1
    }

    /// A service principal's scoped key bound to `workspace`.
    pub fn key(&self, service: &str, workspace: &str, scopes: &[Scope]) -> Caller {
        let issued = self
            .credentials
            .issue_key(&who(service), &ws(workspace), scopes.to_vec())
            .unwrap();
        self.caller(&issued)
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
