//! The credential module's own SQLite file, separate from the main store and
//! its migration chain. It carries its own version table and `application_id`,
//! and a foreign or newer file is refused before anything is written to it,
//! including the switch to WAL.
//!
//! The file is created owner-only (0600) without following links; an existing
//! file that is a link, not a regular file, not owned by this user or readable
//! by anyone else is refused, and so is an existing `-wal` or `-shm` file that
//! fails the same test. SQLite gives new sidecars the main file's permissions.
//! `secure_delete` zeroes freed content, and opening the file and each write
//! that drops secret material end with a truncating WAL checkpoint.
//!
//! Secrets are stored in plaintext inside that file; there is no keychain or
//! at-rest encryption.
use super::CredentialError;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const SCHEMA_VERSION: i64 = 1;

/// Marks a file as a credential database in the SQLite header ("FCRD").
const APPLICATION_ID: i64 = 0x4643_5244;

/// How long a writer waits for another connection's transaction to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a purge checkpoint waits for other connections before leaving the
/// WAL to a later checkpoint.
const PURGE_TIMEOUT: Duration = Duration::from_millis(100);

const SCHEMA: &str = "
CREATE TABLE credential_schema_version (version INTEGER NOT NULL);
CREATE TABLE credential_slots (
    workspace TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    account_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    secret BLOB NOT NULL,
    connected_by TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    failure_code TEXT,
    failed_at_ms INTEGER,
    PRIMARY KEY (workspace, provider_id, account_key)
) WITHOUT ROWID;
CREATE TABLE credential_config (
    workspace TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    account_key TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (workspace, provider_id, account_key, name),
    FOREIGN KEY (workspace, provider_id, account_key)
        REFERENCES credential_slots (workspace, provider_id, account_key) ON DELETE CASCADE
) WITHOUT ROWID;
";

pub(crate) trait Storage<T> {
    fn storage(self) -> Result<T, CredentialError>;
}

impl<T> Storage<T> for rusqlite::Result<T> {
    /// SQLite messages are dropped: only a static label leaves this module.
    fn storage(self) -> Result<T, CredentialError> {
        self.map_err(|_| CredentialError::unavailable("credential database error"))
    }
}

pub(crate) fn open(path: &Path) -> Result<Connection, CredentialError> {
    // SQLite's no-follow flag refuses a link anywhere in the path, so the
    // caller's directory is resolved first; the file name itself never is.
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(CredentialError::invalid("path"));
    };
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    let path = directory
        .canonicalize()
        .map_err(|_| CredentialError::unavailable("cannot resolve credential directory"))?
        .join(name);
    ensure_private_file(&path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(&path, flags).storage()?;
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    // Read-only inspection first, so a foreign or newer file stays untouched.
    inspect(&connection)?;
    enable_wal(&connection)?;
    let connection = prepare(connection)?;
    // Clears WAL pages a crash may have left, including removed secrets. A
    // concurrent reader only delays that to a later write's checkpoint.
    purge_wal(&connection)?;
    Ok(connection)
}

/// Creates the main file owner-only, or checks an existing one, then checks
/// any `-wal` and `-shm` file already present the same way: SQLite reuses
/// those as they are, so a planted or shared sidecar would expose its pages.
#[cfg(unix)]
fn ensure_private_file(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::OpenOptionsExt;
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    match created {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => check_private(path)?,
        Err(_) => {
            return Err(CredentialError::unavailable(
                "cannot create credential file",
            ));
        }
    }
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match std::fs::symlink_metadata(&sidecar) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            _ => check_private(Path::new(&sidecar))?,
        }
    }
    Ok(())
}

/// A regular file, not a link, owned by this user, with no group or other
/// permission bits.
#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| CredentialError::unavailable("cannot inspect credential file"))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let owner = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(CredentialError::unavailable(
            "credential file is not private",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_file(_path: &Path) -> Result<(), CredentialError> {
    Err(CredentialError::unavailable(
        "owner-only files need a Unix platform",
    ))
}

/// Whether the file is empty (`false`) or already this module's database at
/// the supported version (`true`). Anything else is refused.
fn inspect(connection: &Connection) -> Result<bool, CredentialError> {
    // One statement reads both from the same snapshot, so a concurrent
    // creator's commit cannot fall between them.
    let identity: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT application_id FROM pragma_application_id()),
                    (SELECT count(*) FROM sqlite_master)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .storage()?;
    match identity {
        (0, 0) => Ok(false),
        (APPLICATION_ID, _) => {
            let version: Option<i64> = connection
                .query_row("SELECT version FROM credential_schema_version", [], |row| {
                    row.get(0)
                })
                .optional()
                .storage()?;
            match version {
                Some(SCHEMA_VERSION) => Ok(true),
                Some(found) if found > SCHEMA_VERSION => Err(CredentialError::unavailable(
                    "credential database is from a newer version",
                )),
                _ => Err(CredentialError::unavailable(
                    "unsupported credential schema",
                )),
            }
        }
        _ => Err(CredentialError::unavailable("not a credential database")),
    }
}

/// Switching a fresh file to WAL needs an exclusive lock and bypasses the busy
/// handler, so concurrent openers retry here for up to the busy timeout. The
/// mode SQLite reports back must be `wal`; anything else is refused.
fn enable_wal(connection: &Connection) -> Result<(), CredentialError> {
    let started = Instant::now();
    loop {
        let mode =
            connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        match mode {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(_) => return Err(CredentialError::unavailable("journal mode is not wal")),
            Err(error) if is_busy(&error) && started.elapsed() < BUSY_TIMEOUT => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(CredentialError::unavailable("cannot enable wal")),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Creates the schema in an empty file under `BEGIN IMMEDIATE`, re-inspecting
/// inside the transaction because another opener may have created it first.
fn prepare(mut connection: Connection) -> Result<Connection, CredentialError> {
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")
        .storage()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    if !inspect(&transaction)? {
        transaction.execute_batch(SCHEMA).storage()?;
        transaction
            .execute(
                "INSERT INTO credential_schema_version (version) VALUES (?1)",
                [SCHEMA_VERSION],
            )
            .storage()?;
        transaction
            .execute_batch(&format!("PRAGMA application_id = {APPLICATION_ID};"))
            .storage()?;
    }
    transaction.commit().storage()?;
    Ok(connection)
}

/// Copies committed WAL frames into the file and truncates the WAL, so no old
/// page holding removed secret material survives there. Returns `false` when a
/// concurrent reader kept the checkpoint from completing. The checkpoint waits
/// at most [`PURGE_TIMEOUT`] for other connections, so a long reader cannot
/// stall the store; the normal busy timeout is restored afterwards.
pub(crate) fn purge_wal(connection: &Connection) -> Result<bool, CredentialError> {
    connection.busy_timeout(PURGE_TIMEOUT).storage()?;
    let busy = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    });
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    match busy {
        Ok(busy) => Ok(busy == 0),
        Err(error) if is_busy(&error) => Ok(false),
        Err(_) => Err(CredentialError::unavailable("credential database error")),
    }
}

pub(crate) fn to_sql(value: u64) -> Result<i64, CredentialError> {
    i64::try_from(value).map_err(|_| CredentialError::invalid("timestamp"))
}

pub(crate) fn from_sql(value: i64) -> Result<u64, CredentialError> {
    u64::try_from(value).map_err(|_| CredentialError::unavailable("negative stored value"))
}
