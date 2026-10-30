//! Private Unix IPC listener, owned together with the state-directory lease.
//! Roots are supplied by the caller; private per-listener directories are created.
use crate::{NetworkError, Result, daemon_lease::DaemonLease};
use cap_std::{ambient_authority, fs::Dir};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};
use tokio::net::UnixStream;

mod ownership;
use ownership::SocketPath;

/// Owns a state lease and, for a distinct socket root, a namespace lease.
/// Fixed-path replacements are preserved; unexpected entries that cannot be
/// restored without overwriting are retained in quarantine and reported.
/// Drop reports cleanup failures to stderr because it cannot return an error.
pub struct LocalListener {
    // Field order keeps namespace cleanup ahead of both lease releases.
    socket: SocketPath,
    _socket_lease: Option<DaemonLease>,
    _lease: DaemonLease,
}

impl LocalListener {
    /// Only a refused connection establishes a stale socket. Live, inaccessible
    /// or unresponsive sockets are preserved. The lease must cover this root.
    pub async fn bind(root: &Path, lease: DaemonLease, timeout: Duration) -> Result<Self> {
        Self::bind_at(root, root, lease, timeout).await
    }

    /// A separately selected private socket directory supports state paths that
    /// exceed the native Unix socket address limit. Both directories must exist;
    /// no shared temporary directory, symlink alias or global discovery is created.
    /// The private staging path needs eight more address bytes than daemon.sock.
    /// Publication requires exclusive rename support (Linux or macOS).
    pub async fn bind_at(
        root: &Path,
        socket_directory: &Path,
        lease: DaemonLease,
        timeout: Duration,
    ) -> Result<Self> {
        if !root.is_absolute() || !socket_directory.is_absolute() || timeout.is_zero() {
            return Err(NetworkError::Invalid(
                "absolute state root and positive timeout required",
            ));
        }
        let canonical = root.canonicalize()?;
        let directory = Dir::open_ambient_dir(&canonical, ambient_authority())?;
        lease
            .check_root(&directory)
            .map_err(|_| NetworkError::Invalid("state lease does not cover root"))?;
        private_root(&canonical)?;
        let socket_directory = socket_directory.canonicalize()?;
        private_root(&socket_directory)?;
        let socket_root_identity = identity(&fs::symlink_metadata(&socket_directory)?);
        let socket_root = Dir::open_ambient_dir(&socket_directory, ambient_authority())?;
        // The same directory is already locked. A distinct namespace must have
        // its own lease even when different state roots select it.
        let socket_lease = if identity(&fs::symlink_metadata(&canonical)?) == socket_root_identity {
            None
        } else {
            Some(DaemonLease::acquire(&socket_root).map_err(|error| {
                std::io::Error::other(format!("IPC socket directory lease: {error}"))
            })?)
        };
        let mut socket = SocketPath::new(&socket_directory)?;
        let path = &socket.canonical;
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                private_socket(&metadata)?;
                socket.recover_existing(timeout).await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        socket.bind()?;
        let socket_identity = socket.identity()?;
        socket.publish()?;
        {
            let current = fs::symlink_metadata(&socket.canonical)?;
            private_socket(&current)?;
            if identity(&current) != socket_identity {
                return Err(NetworkError::Invalid("socket changed during startup"));
            }
            private_root(&canonical)?;
            let current_root = Dir::open_ambient_dir(&canonical, ambient_authority())?;
            lease
                .check_root(&current_root)
                .map_err(|_| NetworkError::Invalid("state root changed during startup"))?;
            private_root(&socket_directory)?;
            if identity(&fs::symlink_metadata(&socket_directory)?) != socket_root_identity {
                return Err(NetworkError::Invalid(
                    "socket directory changed during startup",
                ));
            }
        }
        Ok(Self {
            socket,
            _socket_lease: socket_lease,
            _lease: lease,
        })
    }

    pub fn path(&self) -> &Path {
        &self.socket.canonical
    }

    /// The UID check authenticates the local account, not a registered session.
    /// Session credentials and conversation ownership are separate checks.
    pub async fn accept(&self, timeout: Duration) -> Result<UnixStream> {
        if timeout.is_zero() {
            return Err(NetworkError::Invalid(
                "positive IPC accept timeout required",
            ));
        }
        let (stream, _) = tokio::time::timeout(timeout, self.socket.listener()?.accept())
            .await
            .map_err(|_| NetworkError::TimedOut)??;
        // geteuid has no pointer arguments or side effects.
        if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            return Err(NetworkError::Invalid("IPC caller account differs"));
        }
        Ok(stream)
    }
}
fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
fn private_root(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(NetworkError::Invalid("IPC root must be privately owned"));
    }
    Ok(())
}
fn private_socket(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_socket()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(NetworkError::Invalid("IPC socket must be privately owned"));
    }
    Ok(())
}
