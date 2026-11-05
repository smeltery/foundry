//! The identity store's own SQLite file. It is separate from the main store and
//! its migration chain, and carries its own schema version table and
//! `application_id`, so it never adopts another program's database. Version 1
//! holds credentials and pairings; version 2 adds the organization tables.
//!
//! On Unix a new file is created owner-only (0600); SQLite gives its journal
//! files the same mode. Elsewhere the platform default applies.
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior};

use super::error::{Storage, StoreError};

const SCHEMA_VERSION: i64 = 2;

/// Marks a file as an identity database in the SQLite header ("FIDN").
const APPLICATION_ID: i64 = 0x4649_444E;

/// How long a writer waits for another connection's transaction to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Times are milliseconds since the Unix epoch. Only digests of credential
/// bytes are stored, never the bytes themselves; a pairing's digest is erased
/// (NULL) once it can no longer be claimed. `sequence` orders credentials
/// issued in the same millisecond.
const SCHEMA: &str = "
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
";

/// Version 2: workspaces (a single-use workspace names its paying parent),
/// memberships, invitations, settings namespaces and the audit log. Triggers
/// refuse every update or delete of an audit row.
const ORGANIZATION_STEP: &str = "
CREATE TABLE org_workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    handle TEXT NOT NULL,
    state TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    parent TEXT,
    trial_key TEXT,
    lifetime_minutes INTEGER,
    expires_ms INTEGER,
    retired_ms INTEGER,
    retire_cause TEXT,
    UNIQUE (parent, trial_key)
);
CREATE TABLE org_members (
    workspace TEXT NOT NULL,
    principal TEXT NOT NULL,
    role TEXT NOT NULL,
    email TEXT,
    joined_ms INTEGER NOT NULL,
    PRIMARY KEY (workspace, principal)
);
CREATE TABLE org_invitations (
    sequence INTEGER PRIMARY KEY,
    workspace TEXT NOT NULL,
    email TEXT NOT NULL,
    role TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE TABLE org_settings (
    workspace TEXT NOT NULL,
    namespace TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (workspace, namespace)
);
CREATE TABLE org_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace TEXT NOT NULL,
    action TEXT NOT NULL,
    actor TEXT,
    actor_email TEXT,
    target_kind TEXT NOT NULL,
    target TEXT NOT NULL,
    label TEXT,
    details TEXT NOT NULL,
    personal_to TEXT,
    created_ms INTEGER NOT NULL
);
CREATE INDEX org_audit_by_workspace ON org_audit (workspace, sequence);
CREATE TRIGGER org_audit_no_update BEFORE UPDATE ON org_audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER org_audit_no_delete BEFORE DELETE ON org_audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER org_audit_no_replace BEFORE INSERT ON org_audit
WHEN EXISTS (SELECT 1 FROM org_audit WHERE sequence = NEW.sequence)
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
";

/// The audit log's guards. A version 2 file missing any of them is refused,
/// because its append-only guarantee no longer holds.
const AUDIT_GUARDS: [&str; 4] = [
    "org_audit_by_workspace",
    "org_audit_no_update",
    "org_audit_no_delete",
    "org_audit_no_replace",
];

pub(crate) fn open(path: &Path) -> Result<Connection, StoreError> {
    create_private(path)?;
    let connection = Connection::open(path).storage()?;
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    // Identify the file before switching its journal mode, so a foreign or
    // newer database is refused without being converted to WAL.
    let connection = prepare(connection)?;
    enable_wal(&connection)?;
    Ok(connection)
}

/// An in-memory database has no journal file, so it skips WAL.
pub(crate) fn open_in_memory() -> Result<Connection, StoreError> {
    let connection = Connection::open_in_memory().storage()?;
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    prepare(connection)
}

#[cfg(unix)]
fn create_private(path: &Path) -> Result<(), StoreError> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;
    use std::os::unix::fs::OpenOptionsExt;

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(StoreError::unavailable(format!("create: {error}"))),
    }
}

#[cfg(not(unix))]
fn create_private(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Switching a fresh file to WAL needs an exclusive lock and bypasses the busy
/// handler, so concurrent openers retry here for up to the busy timeout. The
/// mode SQLite reports back must be `wal`; anything else is refused.
fn enable_wal(connection: &Connection) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        let mode =
            connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        match mode {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => {
                return Err(StoreError::unavailable(format!(
                    "journal mode {mode} instead of wal"
                )));
            }
            Err(error) if is_busy(&error) && started.elapsed() < BUSY_TIMEOUT => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(StoreError::unavailable(error.to_string())),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Creates the schema in an empty file, or checks that an existing file is an
/// identity database at the supported version. A file holding anything else,
/// including a newer identity schema, is refused untouched.
fn prepare(mut connection: Connection) -> Result<Connection, StoreError> {
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .storage()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    let application_id: i64 = transaction
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .storage()?;
    let objects: i64 = transaction
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .storage()?;
    match (application_id, objects) {
        (0, 0) => {
            transaction.execute_batch(SCHEMA).storage()?;
            transaction.execute_batch(ORGANIZATION_STEP).storage()?;
            transaction
                .execute(
                    "INSERT INTO identity_schema_version (version) VALUES (?1)",
                    [SCHEMA_VERSION],
                )
                .storage()?;
            transaction
                .execute_batch(&format!("PRAGMA application_id = {APPLICATION_ID};"))
                .storage()?;
        }
        (APPLICATION_ID, _) => {
            let version: Option<i64> = transaction
                .query_row("SELECT version FROM identity_schema_version", [], |row| {
                    row.get(0)
                })
                .optional()
                .storage()?;
            match version {
                Some(SCHEMA_VERSION) => {}
                // The step and the version bump commit together or not at all.
                Some(1) => {
                    transaction.execute_batch(ORGANIZATION_STEP).storage()?;
                    transaction
                        .execute(
                            "UPDATE identity_schema_version SET version = ?1",
                            [SCHEMA_VERSION],
                        )
                        .storage()?;
                }
                _ => {
                    let found = version.map_or("none".to_owned(), |version| version.to_string());
                    return Err(StoreError::unavailable(format!(
                        "unsupported schema version {found}"
                    )));
                }
            }
            check_audit_guards(&transaction)?;
        }
        _ => return Err(StoreError::unavailable("not an identity database")),
    }
    transaction.commit().storage()?;
    Ok(connection)
}

fn check_audit_guards(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    for name in AUDIT_GUARDS {
        let present: bool = transaction
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master
                 WHERE name = ?1 AND tbl_name = 'org_audit')",
                [name],
                |row| row.get(0),
            )
            .storage()?;
        if !present {
            return Err(StoreError::unavailable(format!(
                "missing audit guard {name}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn to_sql(ms: u64) -> Result<i64, StoreError> {
    i64::try_from(ms).map_err(|_| StoreError::InvalidRequest { field: "time" })
}

pub(crate) fn from_sql(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::unavailable("negative stored value"))
}

/// `now + lifetime`, refusing a zero or unrepresentable lifetime.
pub(crate) fn expiry(
    now_ms: u64,
    lifetime: Duration,
    field: &'static str,
) -> Result<i64, StoreError> {
    let lifetime_ms = u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX);
    let expires = now_ms
        .checked_add(lifetime_ms)
        .filter(|_| lifetime_ms > 0)
        .ok_or(StoreError::InvalidRequest { field })?;
    i64::try_from(expires).map_err(|_| StoreError::InvalidRequest { field })
}
