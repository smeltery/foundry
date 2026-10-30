use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    fs::File,
    io::{self, Seek, Write},
    os::unix::fs::MetadataExt as _,
};

/// Unpublished bytes. The pathname is removed before any payload is written.
pub(super) struct Spool {
    file: File,
    bytes: u64,
    maximum: u64,
    sealed: bool,
}
impl Spool {
    pub fn create(root: &Dir, maximum: usize) -> io::Result<Self> {
        let metadata = root.dir_metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(io::Error::other("reply root protection failed"));
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| io::Error::other("reply entropy failed"))?;
        let name = format!(".reply-{}", data_encoding::HEXLOWER.encode(&random));
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        let file = root.open_with(&name, &options)?.into_std();
        let held = file.metadata()?;
        let named = root.symlink_metadata(&name)?;
        if !held.is_file()
            || held.uid() != unsafe { libc::geteuid() }
            || held.mode() & 0o7777 != 0o600
            || held.nlink() != 1
            || (held.dev(), held.ino()) != (named.dev(), named.ino())
        {
            return Err(io::Error::other("reply file protection failed"));
        }
        root.remove_file(&name)?;
        Ok(Self {
            file,
            bytes: 0,
            maximum: maximum as u64,
            sealed: false,
        })
    }
    pub fn seal(&mut self) -> io::Result<()> {
        if self.sealed {
            return Err(io::Error::other("reply already sealed"));
        }
        self.file.flush()?;
        self.file.sync_all()?;
        if self.file.metadata()?.len() != self.bytes {
            return Err(io::Error::other("reply size changed"));
        }
        self.file.rewind()?;
        self.sealed = true;
        Ok(())
    }
    pub fn into_file(self, expected: u64) -> io::Result<File> {
        if !self.sealed || self.bytes != expected {
            return Err(io::Error::other("reply was not prepared"));
        }
        Ok(self.file)
    }
}
impl Write for Spool {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.sealed || bytes.len() as u64 > self.maximum.saturating_sub(self.bytes) {
            return Err(io::Error::other("reply exceeds bound or is sealed"));
        }
        let written = self.file.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl crate::daemon_engine::InboxOutput for Spool {
    fn seal(&mut self) -> io::Result<()> {
        Self::seal(self)
    }
}

#[cfg(test)]
#[path = "spool_tests.rs"]
mod tests;
