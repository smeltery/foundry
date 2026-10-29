use super::SnapshotFailure as Failure;
use cap_std::fs::{Dir, MetadataExt};
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
};

type Result<T> = std::result::Result<T, Failure>;

pub(super) fn outside(source: &Dir, destination: &Dir) -> Result<()> {
    let source = source.dir_metadata()?;
    let mut current = destination.try_clone()?.into_std_file();
    // Compare real directory identities, not path-prefix strings or symlink aliases.
    loop {
        use std::os::unix::fs::MetadataExt;
        let metadata = current.metadata()?;
        if metadata.dev() == source.dev() && metadata.ino() == source.ino() {
            return Err(Failure::Invalid);
        }
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Failure::Io);
        }
        let parent = unsafe { std::fs::File::from_raw_fd(fd) };
        let parent_metadata = parent.metadata()?;
        if metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino() {
            return Ok(());
        }
        current = parent;
    }
}
pub(super) fn private(dir: &Dir) -> Result<()> {
    let metadata = dir.dir_metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o7777 != 0o700 {
        return Err(Failure::Invalid);
    }
    Ok(())
}
pub(super) fn stage(parent: &Dir, name: &str) -> Result<Dir> {
    let name = CString::new(name).map_err(|_| Failure::Invalid)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(Failure::Io);
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(Failure::Cleanup);
        }
        return Err(Failure::Io);
    }
    Ok(Dir::from_std_file(unsafe {
        std::fs::File::from_raw_fd(fd)
    }))
}
pub(super) fn publish(parent: &Dir, stage: &str, final_name: &str) -> Result<()> {
    let from = CString::new(stage).map_err(|_| Failure::Invalid)?;
    let to = CString::new(final_name).map_err(|_| Failure::Invalid)?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(Failure::Unsupported);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if result != 0 {
        return Err(Failure::Io);
    }
    foundry_tools::workspace_snapshot::sync_directory(parent)?;
    Ok(())
}
pub(super) fn discard(parent: &Dir, name: &str) -> Result<()> {
    match parent.remove_dir_all(Path::new(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Failure::Cleanup),
    }
}
