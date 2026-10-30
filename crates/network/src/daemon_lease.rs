//! Single daemon ownership within a caller-supplied private state directory.
//! The lock file is retained across shutdown; unlinking it could split ownership.
use cap_std::fs::Dir;
use std::fmt;

pub struct DaemonLease {
    _file: std::fs::File,
    #[cfg(unix)]
    root_identity: (u64, u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseError {
    AlreadyRunning,
    UnsafeStorage,
    Storage,
    Unsupported,
}
impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyRunning => "daemon already owns the state directory",
            Self::UnsafeStorage => "daemon state protection failed",
            Self::Storage => "daemon lock I/O failed",
            Self::Unsupported => "daemon ownership unsupported on this platform",
        })
    }
}
impl std::error::Error for LeaseError {}

impl DaemonLease {
    /// Duplicate the locked open-file description, retaining ownership until the
    /// last guard closes. No lock file is reopened or unlocked independently.
    #[cfg(unix)]
    pub(crate) fn retain(&self) -> Result<Self, LeaseError> {
        Ok(Self {
            _file: self._file.try_clone().map_err(|_| LeaseError::Storage)?,
            #[cfg(unix)]
            root_identity: self.root_identity,
        })
    }
    #[cfg(unix)]
    pub(crate) fn check_root(&self, root: &Dir) -> Result<(), LeaseError> {
        use cap_std::fs::MetadataExt;
        let metadata = root.dir_metadata().map_err(|_| LeaseError::Storage)?;
        if (metadata.dev(), metadata.ino()) != self.root_identity {
            return Err(LeaseError::UnsafeStorage);
        }
        Ok(())
    }
    /// Ownership lasts until drop, including process termination. This does not
    /// authorize IPC callers or protect against another process of the same user.
    pub fn acquire(root: &Dir) -> Result<Self, LeaseError> {
        #[cfg(unix)]
        return unix::acquire(root);
        #[cfg(not(unix))]
        {
            let _ = root;
            Err(LeaseError::Unsupported)
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    use cap_std::fs::{Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
    use std::os::unix::fs::MetadataExt as StdMetadataExt;
    use std::{fs::TryLockError, io::ErrorKind};

    const NAME: &str = "daemon.lock";
    fn storage<T>(result: std::io::Result<T>) -> Result<T, LeaseError> {
        result.map_err(|_| LeaseError::Storage)
    }
    fn private(metadata: &Metadata, directory: bool) -> Result<(), LeaseError> {
        // geteuid reads process identity without pointer arguments or side effects.
        let owner = unsafe { libc::geteuid() };
        if metadata.uid() != owner
            || metadata.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file() || metadata.nlink() != 1
            }
        {
            return Err(LeaseError::UnsafeStorage);
        }
        Ok(())
    }
    pub(super) fn acquire(root: &Dir) -> Result<DaemonLease, LeaseError> {
        private(&storage(root.dir_metadata())?, true)?;
        match root.symlink_metadata(NAME) {
            Ok(metadata) => private(&metadata, false)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(LeaseError::Storage),
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK)
            .follow(FollowSymlinks::No);
        let file = match root.open_with(NAME, options.clone().create_new(true)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                storage(root.open_with(NAME, &options))?
            }
            Err(_) => return Err(LeaseError::Storage),
        };
        private(&storage(file.metadata())?, false)?;
        let file = file.into_std();
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LeaseError::AlreadyRunning),
            Err(TryLockError::Error(_)) => return Err(LeaseError::Storage),
        }
        let held = storage(file.metadata())?;
        let named = storage(root.symlink_metadata(NAME))?;
        private(&named, false)?;
        if held.dev() != named.dev() || held.ino() != named.ino() || held.nlink() != 1 {
            return Err(LeaseError::UnsafeStorage);
        }
        let directory = storage(root.dir_metadata())?;
        Ok(DaemonLease {
            _file: file,
            root_identity: (directory.dev(), directory.ino()),
        })
    }
}
