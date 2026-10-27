use super::*;
use std::{fs::File, path::PathBuf};
const APPLICATION: i64 = 0x46354d50;
const TABLE: &str = "CREATE TABLE projection (id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL CHECK(revision>0),payload BLOB NOT NULL)";

pub(super) fn schema(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if app == 0 && version == 0 && objects == 0 {
        tx.execute_batch(TABLE)?;
        tx.pragma_update(None, "application_id", APPLICATION)?;
        tx.pragma_update(None, "user_version", 1)?;
    }
    verify_schema(&tx)?;
    tx.commit()?;
    Ok(())
}
pub(super) fn verify_schema(connection: &Connection) -> Result<()> {
    let app: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != APPLICATION || version != 1 {
        return Err(PersistenceError::Schema);
    }
    let sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='projection'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| PersistenceError::Schema)?;
    let count: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0))?;
    if sql != TABLE || count != 1 {
        return Err(PersistenceError::Schema);
    }
    Ok(())
}

pub(crate) enum OpenMode {
    Create,
    Existing,
    CreateOrOpen,
}

pub(crate) struct Protection {
    root: PathBuf,
    name: String,
    database: PathBuf,
    #[cfg(unix)]
    root_file: File,
    #[cfg(unix)]
    database_file: File,
}
impl Protection {
    pub fn path(&self) -> &Path {
        &self.database
    }
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_named(root, MEMBERSHIP_DATABASE, OpenMode::CreateOrOpen)
    }
    #[cfg(not(unix))]
    pub fn open_named(_: &Path, _: &str, _: OpenMode) -> Result<Self> {
        Err(PersistenceError::Unsupported)
    }
    #[cfg(not(unix))]
    pub fn check(&self) -> Result<()> {
        Err(PersistenceError::Unsupported)
    }
    #[cfg(not(unix))]
    pub fn sync(&self) -> Result<()> {
        Err(PersistenceError::Unsupported)
    }

    #[cfg(unix)]
    pub fn open_named(root: &Path, name: &str, mode: OpenMode) -> Result<Self> {
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(PersistenceError::Invalid);
        }
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
        let root_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(|_| PersistenceError::UnsafeStorage)?;
        private(
            &root_file
                .metadata()
                .map_err(|_| PersistenceError::Storage)?,
            true,
        )?;
        // SQLite NOFOLLOW rejects symlinks in ancestors too (for example system
        // temporary-directory aliases). Resolve ancestors only after opening and
        // validating the supplied final directory without following a link.
        let root = std::fs::canonicalize(root).map_err(|_| PersistenceError::Storage)?;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            inspect(&root.join(format!("{name}{suffix}")))?;
        }
        let database = root.join(name);
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let opened = match mode {
            OpenMode::Existing => options.open(&database),
            OpenMode::Create | OpenMode::CreateOrOpen => {
                options.clone().create_new(true).open(&database)
            }
        };
        let database_file = match opened {
            Ok(file) => file,
            Err(error)
                if matches!(mode, OpenMode::CreateOrOpen)
                    && error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                options
                    .open(&database)
                    .map_err(|_| PersistenceError::Storage)?
            }
            Err(_) => return Err(PersistenceError::Storage),
        };
        private(
            &database_file
                .metadata()
                .map_err(|_| PersistenceError::Storage)?,
            false,
        )?;
        let value = Self {
            root,
            name: name.into(),
            database,
            root_file,
            database_file,
        };
        value.check()?;
        Ok(value)
    }
    #[cfg(unix)]
    pub fn check(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        for (path, file, directory) in [
            (&self.root, &self.root_file, true),
            (&self.database, &self.database_file, false),
        ] {
            let path_meta =
                std::fs::symlink_metadata(path).map_err(|_| PersistenceError::UnsafeStorage)?;
            let held = file.metadata().map_err(|_| PersistenceError::Storage)?;
            private(&path_meta, directory)?;
            private(&held, directory)?;
            if path_meta.dev() != held.dev() || path_meta.ino() != held.ino() {
                return Err(PersistenceError::UnsafeStorage);
            }
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            inspect(&self.root.join(format!("{}{suffix}", self.name)))?;
        }
        Ok(())
    }
    #[cfg(unix)]
    pub fn sync(&self) -> Result<()> {
        self.check()?;
        self.root_file
            .sync_all()
            .map_err(|_| PersistenceError::Storage)
    }
}
#[cfg(unix)]
fn private(metadata: &std::fs::Metadata, directory: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(PersistenceError::UnsafeStorage);
    }
    Ok(())
}
#[cfg(unix)]
fn inspect(path: &Path) -> Result<()> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => private(&metadata, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(PersistenceError::Storage),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| PersistenceError::UnsafeStorage)?;
    private(
        &file.metadata().map_err(|_| PersistenceError::Storage)?,
        false,
    )
}

/// Cooperating operations serialize on a separately protected lock-file inode.
pub(crate) struct StorageLock(File);
impl Drop for StorageLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
impl Protection {
    #[cfg(unix)]
    pub(crate) fn lock(&self, timeout: Duration) -> Result<StorageLock> {
        use std::{fs::TryLockError, time::Instant};
        let file = self
            .database_file
            .try_clone()
            .map_err(|_| PersistenceError::Storage)?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    let guard = StorageLock(file);
                    self.check()?;
                    return Ok(guard);
                }
                Err(TryLockError::WouldBlock) if start.elapsed() < timeout => std::thread::sleep(
                    Duration::from_millis(2).min(timeout.saturating_sub(start.elapsed())),
                ),
                Err(TryLockError::WouldBlock) => return Err(PersistenceError::Busy),
                Err(_) => return Err(PersistenceError::Storage),
            }
        }
    }
    #[cfg(not(unix))]
    pub(crate) fn lock(&self, _: Duration) -> Result<StorageLock> {
        Err(PersistenceError::Unsupported)
    }
}

impl Protection {
    #[cfg(unix)]
    pub(crate) fn sync_database(&self) -> Result<()> {
        self.check()?;
        self.database_file
            .sync_all()
            .map_err(|_| PersistenceError::Storage)?;
        self.sync()
    }
    #[cfg(not(unix))]
    pub(crate) fn sync_database(&self) -> Result<()> {
        Err(PersistenceError::Unsupported)
    }
}
