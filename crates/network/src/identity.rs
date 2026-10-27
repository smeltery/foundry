//! Durable local endpoint identity in an explicitly supplied private directory.
//! Unix protection only; unsupported platforms fail without touching storage.
use cap_std::fs::Dir;
use iroh::{EndpointId, SecretKey};
use std::{fmt, time::Duration};

#[cfg(unix)]
const KEY: &str = "machine.key";
#[cfg(unix)]
const LOCK: &str = "identity.lock";

pub struct LocalIdentity {
    key: SecretKey,
}
impl LocalIdentity {
    pub fn endpoint_id(&self) -> EndpointId {
        self.key.public()
    }
    pub fn secret_key(&self) -> &SecretKey {
        &self.key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    InvalidTimeout,
    UnsafeStorage,
    CorruptKey,
    LockTimeout,
    Storage,
    Unsupported,
}
impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTimeout => "invalid identity lock timeout",
            Self::UnsafeStorage => "identity storage protection failed",
            Self::CorruptKey => "invalid stored identity",
            Self::LockTimeout => "identity lock timed out",
            Self::Storage => "identity storage operation failed",
            Self::Unsupported => "identity storage unsupported on this platform",
        })
    }
}
impl std::error::Error for IdentityError {}
type Result<T> = std::result::Result<T, IdentityError>;

/// The lock timeout must be positive and at most 60 seconds. It bounds lock
/// contention, not kernel I/O or scheduling. All cooperating writers use this API.
/// The directory must already exist, be owned by the effective user, and have
/// mode 0700. A capability does not prove how its caller obtained the directory.
/// No protection against a malicious same-user process or disk rollback is claimed.
pub fn load_or_create_identity(root: &Dir, lock_timeout: Duration) -> Result<LocalIdentity> {
    if lock_timeout.is_zero() || lock_timeout > Duration::from_secs(60) {
        return Err(IdentityError::InvalidTimeout);
    }
    #[cfg(unix)]
    return unix::load(root, lock_timeout);
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(IdentityError::Unsupported)
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    use cap_std::fs::{Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
    use std::{
        fs::{File, TryLockError},
        io::{ErrorKind, Read, Write},
        thread,
        time::Instant,
    };

    fn storage<T>(value: std::io::Result<T>) -> Result<T> {
        value.map_err(|_| IdentityError::Storage)
    }
    fn options() -> OpenOptions {
        let mut options = OpenOptions::new();
        options.follow(FollowSymlinks::No);
        options.mode(0o600).custom_flags(libc::O_NONBLOCK);
        options
    }
    fn private(metadata: &Metadata, directory: bool) -> Result<()> {
        // Reading the effective user ID has no pointers or side effects.
        let owner = unsafe { libc::geteuid() };
        let expected = if directory { 0o700 } else { 0o600 };
        if metadata.uid() != owner
            || metadata.mode() & 0o7777 != expected
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return Err(IdentityError::UnsafeStorage);
        }
        Ok(())
    }
    fn inspect(root: &Dir, name: &str) -> Result<bool> {
        match root.symlink_metadata(name) {
            Ok(metadata) => {
                private(&metadata, false)?;
                if metadata.nlink() != 1 {
                    return Err(IdentityError::UnsafeStorage);
                }
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(_) => Err(IdentityError::Storage),
        }
    }
    fn lock(root: &Dir, timeout: Duration) -> Result<File> {
        let existing = inspect(root, LOCK)?;
        let open_existing = || root.open_with(LOCK, options().read(true).write(true));
        let file = if existing {
            storage(open_existing())?
        } else {
            match root.open_with(LOCK, options().read(true).write(true).create_new(true)) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => storage(open_existing())?,
                Err(_) => return Err(IdentityError::Storage),
            }
        };
        let metadata = storage(file.metadata())?;
        private(&metadata, false)?;
        if metadata.nlink() != 1 {
            return Err(IdentityError::UnsafeStorage);
        }
        let file = file.into_std();
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(IdentityError::InvalidTimeout)?;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(IdentityError::LockTimeout);
                    }
                    thread::sleep(remaining.min(Duration::from_millis(5)));
                }
                Err(TryLockError::Error(_)) => return Err(IdentityError::Storage),
            }
        }
    }
    fn read(root: &Dir) -> Result<Option<LocalIdentity>> {
        match root.symlink_metadata(KEY) {
            Ok(metadata) => private(&metadata, false)?,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(IdentityError::Storage),
        }
        let mut file = storage(root.open_with(KEY, options().read(true)))?;
        let metadata = storage(file.metadata())?;
        private(&metadata, false)?;
        if metadata.len() != 32 {
            return Err(IdentityError::CorruptKey);
        }
        let mut bytes = [0; 32];
        storage(file.read_exact(&mut bytes))?;
        let mut extra = [0];
        if storage(file.read(&mut extra))? != 0 {
            return Err(IdentityError::CorruptKey);
        }
        let identity = LocalIdentity {
            key: SecretKey::from_bytes(&bytes),
        };
        match metadata.nlink() {
            1 => {}
            2 => recover_link(root, &file, &identity)?,
            _ => return Err(IdentityError::UnsafeStorage),
        }
        // A prior publisher could have failed after publication but before sync.
        storage(file.sync_all())?;
        Ok(Some(identity))
    }
    fn recover_link(root: &Dir, key: &cap_std::fs::File, identity: &LocalIdentity) -> Result<()> {
        let name = format!(".identity-{}.tmp", identity.endpoint_id());
        let key_metadata = storage(key.metadata())?;
        let temporary = root
            .symlink_metadata(&name)
            .map_err(|_| IdentityError::UnsafeStorage)?;
        private(&temporary, false)?;
        if key_metadata.nlink() != 2
            || temporary.nlink() != 2
            || key_metadata.dev() != temporary.dev()
            || key_metadata.ino() != temporary.ino()
        {
            return Err(IdentityError::UnsafeStorage);
        }
        // Only our exact publication residue is removed, while the lock is held.
        storage(root.remove_file(&name))?;
        if storage(key.metadata())?.nlink() != 1 {
            return Err(IdentityError::UnsafeStorage);
        }
        Ok(())
    }
    pub(super) fn load(root: &Dir, timeout: Duration) -> Result<LocalIdentity> {
        private(&storage(root.dir_metadata())?, true)?;
        let _lock = lock(root, timeout)?;
        let directory = storage(
            root.open_with(
                ".",
                options()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK),
            ),
        )?
        .into_std();
        if let Some(identity) = read(root)? {
            storage(directory.sync_all())?;
            return Ok(identity);
        }
        let identity = LocalIdentity {
            key: SecretKey::generate(),
        };
        let temporary = format!(".identity-{}.tmp", identity.endpoint_id());
        let mut file = storage(root.open_with(&temporary, options().write(true).create_new(true)))?;
        let result = (|| {
            private(&storage(file.metadata())?, false)?;
            storage(file.write_all(&identity.key.to_bytes()))?;
            storage(file.sync_all())?;
            publish(root, &temporary, &directory)
        })();
        if result.is_err() {
            // Never remove the published key or a pre-existing temporary file.
            let _ = root.remove_file(&temporary);
        }
        result?;
        Ok(identity)
    }
    fn publish(root: &Dir, temporary: &str, directory: &File) -> Result<()> {
        // link is atomic and refuses replacement, even if a noncooperating writer
        // inserted a destination since the missing-key check. No rename-overwrite.
        storage(root.hard_link(temporary, root, KEY))?;
        storage(root.remove_file(temporary))?;
        storage(directory.sync_all())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn publication_collision_never_overwrites_destination() {
            let path = std::env::temp_dir().join(format!(
                "identity-publish-{}",
                SecretKey::generate().public()
            ));
            std::fs::create_dir(&path).unwrap();
            let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
            root.write(KEY, b"existing").unwrap();
            root.write("pending", [4; 32]).unwrap();
            let directory = root.try_clone().unwrap().into_std_file();
            assert_eq!(
                publish(&root, "pending", &directory),
                Err(IdentityError::Storage)
            );
            assert_eq!(root.read(KEY).unwrap(), b"existing");
            assert_eq!(root.read("pending").unwrap(), [4; 32]);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
