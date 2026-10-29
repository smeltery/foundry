use super::*;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use std::{fs::File, os::unix::fs::MetadataExt};

pub(super) fn open_file(dir: &Dir, name: &str, writable: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_NONBLOCK);
    dir.open_with(name, &options)
        .map(|f| f.into_std())
        .map_err(|_| AttachmentError::UnsafePath)
}
pub(super) fn create_file(dir: &Dir, name: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No);
    Ok(dir.open_with(name, &options)?.into_std())
}
pub(super) fn private(file: &File) -> Result<()> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.mode() & 0o7777 != 0o600
        || m.uid() != unsafe { libc::geteuid() }
        || m.nlink() != 1
    {
        return Err(AttachmentError::UnsafePath);
    }
    Ok(())
}
pub(super) fn directory(root: &Dir, name: &str, create: bool) -> Result<Dir> {
    if create {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match root.create_dir_with(name, &builder) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let dir = root
        .open_dir_nofollow(name)
        .map_err(|_| AttachmentError::UnsafePath)?;
    use cap_std::fs::MetadataExt;
    let m = dir.dir_metadata()?;
    if m.mode() & 0o7777 != 0o700 || m.uid() != unsafe { libc::geteuid() } {
        return Err(AttachmentError::UnsafePath);
    }
    Ok(dir)
}
pub(super) fn sync(dir: &Dir) -> Result<()> {
    // A capability directory may use O_PATH on Linux, which cannot be fsynced.
    // Open the same directory relative to its retained descriptor, not its path.
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_DIRECTORY);
    dir.open_with(".", &options)?.sync_all().map_err(Into::into)
}
pub(super) fn exists(dir: &Dir, name: &str) -> Result<bool> {
    match dir.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn mark_download(file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::{
            os::fd::AsRawFd,
            time::{SystemTime, UNIX_EPOCH},
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AttachmentError::Storage)?
            .as_secs();
        let tag = format!("0081;{timestamp:x};Foundry;");
        // The descriptor and attribute bytes remain valid throughout this call.
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c"com.apple.quarantine".as_ptr(),
                tag.as_ptr().cast(),
                tag.len(),
                0,
                0,
            )
        };
        if result != 0 {
            return Err(AttachmentError::Storage);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = file;
    Ok(())
}

#[cfg(test)]
#[path = "filesystem_tests.rs"]
mod tests;
