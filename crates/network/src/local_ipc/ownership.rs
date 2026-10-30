//! Only the unpredictable private directory is trusted against accidental fixed-path mutation.
use super::{identity, private_socket};
use crate::{NetworkError, Result};
use std::{
    ffi::CString,
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};

type Identity = (u64, u64);

pub(super) struct SocketPath {
    pub(super) canonical: PathBuf,
    directory: PathBuf,
    staged: bool,
    published: Option<Identity>,
    // Drop runs ownership cleanup before this socket handle is released.
    listener: Option<UnixListener>,
    bound: Option<Identity>,
}

impl SocketPath {
    pub(super) fn new(root: &Path) -> Result<Self> {
        for _ in 0..8 {
            let mut random = [0; 12];
            getrandom::fill(&mut random).map_err(io::Error::other)?;
            let name = format!(".{}", data_encoding::BASE64URL_NOPAD.encode(&random));
            let directory = root.join(name);
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {
                    let owner = Self {
                        canonical: root.join("daemon.sock"),
                        directory,
                        staged: false,
                        published: None,
                        listener: None,
                        bound: None,
                    };
                    fs::set_permissions(&owner.directory, fs::Permissions::from_mode(0o700))?;
                    return Ok(owner);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(NetworkError::Invalid(
            "cannot allocate private IPC directory",
        ))
    }

    fn staged_path(&self) -> PathBuf {
        self.directory.join("s")
    }

    pub(super) fn bind(&mut self) -> Result<()> {
        let path = self.staged_path();
        let listener = UnixListener::bind(&path)?;
        self.staged = true;
        self.listener = Some(listener);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(&path)?;
        private_socket(&metadata)?;
        self.bound = Some(identity(&metadata));
        Ok(())
    }

    pub(super) fn listener(&self) -> Result<&UnixListener> {
        self.listener
            .as_ref()
            .ok_or(NetworkError::Invalid("IPC socket not bound"))
    }

    pub(super) fn identity(&self) -> Result<Identity> {
        self.bound
            .ok_or(NetworkError::Invalid("IPC socket not bound"))
    }

    pub(super) fn publish(&mut self) -> Result<()> {
        let expected = self.identity()?;
        // No pathname lookup at the public name establishes ownership.
        rename_exclusive(&self.staged_path(), &self.canonical)?;
        self.staged = false;
        self.published = Some(expected);
        Ok(())
    }

    pub(super) async fn recover_existing(&self, timeout: Duration) -> Result<()> {
        self.recover_with(timeout, || {}).await
    }

    async fn recover_with(&self, timeout: Duration, after_move: impl FnOnce()) -> Result<()> {
        let mut moved = self
            .quarantine()?
            .ok_or(NetworkError::Invalid("socket disappeared during recovery"))?;
        after_move();
        // Probe the actual quarantined inode, never a replaceable public name.
        // In particular, no pre-move inode snapshot needs to survive inode reuse.
        let metadata = fs::symlink_metadata(&moved.path)?;
        if let Err(error) = private_socket(&metadata) {
            moved.restore()?;
            return Err(error);
        }
        match tokio::time::timeout(timeout, UnixStream::connect(&moved.path)).await {
            Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => moved.remove(),
            _ => {
                moved.restore()?;
                Err(NetworkError::Invalid("existing socket is not safely stale"))
            }
        }
    }

    fn quarantine(&self) -> Result<Option<Quarantine<'_>>> {
        let path = self.directory.join("q");
        match rename_exclusive(&self.canonical, &path) {
            Ok(()) => Ok(Some(Quarantine {
                canonical: &self.canonical,
                path,
                pending: true,
            })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn remove_expected(&self, expected: Identity) -> Result<bool> {
        self.remove_with(expected, || {})
    }

    fn remove_with(&self, expected: Identity, after_move: impl FnOnce()) -> Result<bool> {
        let Some(mut moved) = self.quarantine()? else {
            return Ok(false);
        };
        after_move();
        // The check is AFTER the move into a private namespace, not stat+unlink
        // on a name another ordinary process might replace. The bound listener
        // remains alive throughout cleanup, retaining the original socket.
        let metadata = fs::symlink_metadata(&moved.path)?;
        if metadata.file_type().is_socket() && identity(&metadata) == expected {
            moved.remove()?;
            return Ok(true);
        }
        moved.restore()?;
        Ok(false)
    }
}

// Restores on cancellation too, while the namespace and state leases are held.
struct Quarantine<'a> {
    canonical: &'a Path,
    path: PathBuf,
    pending: bool,
}
impl Quarantine<'_> {
    fn restore(&mut self) -> Result<()> {
        self.pending = false;
        // An occupied destination is never overwritten. Failed restores stay
        // quarantined for explicit manual recovery.
        rename_exclusive(&self.path, self.canonical).map_err(|error| preserved(&self.path, error))
    }
    fn remove(&mut self) -> Result<()> {
        self.pending = false;
        fs::remove_file(&self.path).map_err(|error| preserved(&self.path, error))
    }
}
impl Drop for Quarantine<'_> {
    fn drop(&mut self) {
        if self.pending
            && let Err(error) = self.restore()
        {
            eprintln!("IPC restore incomplete: {error}");
        }
    }
}

fn preserved(path: &Path, error: io::Error) -> NetworkError {
    io::Error::other(format!(
        "IPC entry preserved at {}: {error}",
        path.display()
    ))
    .into()
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        let result = (|| -> Result<()> {
            if let Some(expected) = self.published.take() {
                self.remove_expected(expected)?;
            }
            if self.staged {
                fs::remove_file(self.staged_path())?;
            }
            // Never recursively remove: a retained quarantine must survive.
            fs::remove_dir(&self.directory)?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "IPC cleanup incomplete at {}: {error}",
                self.directory.display()
            );
        }
    }
}

fn rename_exclusive(source: &Path, destination: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // Both C strings live throughout the call. Unsupported kernels/filesystems
    // fail closed; plain rename would overwrite an occupied destination.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive IPC rename requires Linux or macOS",
        ))
    }
}

#[cfg(test)]
#[path = "ownership_tests.rs"]
mod tests;
