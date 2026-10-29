use super::{Failure, Limits, Reply};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Failure>;

/// Open every component without following symlinks; callers select absolute paths.
pub fn open_absolute(path: &Path) -> Result<Dir> {
    if !path.is_absolute() {
        return Err(Failure::Invalid);
    }
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => dir = dir.open_dir_nofollow(name)?,
            _ => return Err(Failure::Invalid),
        }
    }
    Ok(dir)
}
fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
fn create(dir: &Dir, name: &Path) -> Result<cap_std::fs::File> {
    Ok(dir.open_with(name, options().write(true).create_new(true).mode(0o600))?)
}
fn mkdir(dir: &Dir, name: &Path) -> Result<Dir> {
    dir.create_dir_with(name, DirBuilder::new().mode(0o700))?;
    Ok(dir.open_dir_nofollow(name)?)
}
#[derive(Serialize)]
struct Entry<'a> {
    path: &'a [u8],
    kind: &'static str,
    mode: u32,
    bytes: u64,
    digest: Option<[u8; 32]>,
    target: Option<&'a [u8]>,
}
struct Copy {
    limits: Limits,
    deadline: Instant,
    bytes: u64,
    entries: u64,
    manifest: cap_std::fs::File,
    hash: Sha256,
}
impl Copy {
    fn poll(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            Err(Failure::Limit)
        } else {
            Ok(())
        }
    }
    fn charge(&mut self, count: u64) -> Result<()> {
        self.poll()?;
        self.bytes = self.bytes.checked_add(count).ok_or(Failure::Limit)?;
        if self.bytes > self.limits.bytes {
            return Err(Failure::Limit);
        }
        Ok(())
    }
    fn record(&mut self, entry: Entry<'_>) -> Result<()> {
        let mut bytes = serde_json::to_vec(&entry).map_err(|_| Failure::Invalid)?;
        bytes.push(b'\n');
        self.charge(bytes.len() as u64)?;
        self.manifest.write_all(&bytes)?;
        self.hash.update(&bytes);
        Ok(())
    }
    fn walk(&mut self, source: &Dir, destination: &Dir, relative: &Path, depth: u32) -> Result<()> {
        self.poll()?;
        if depth > self.limits.depth {
            return Err(Failure::Limit);
        }
        let before = source.dir_metadata()?;
        // Stream enumeration; entry count bounds both memory and manifest size.
        for entry in source.entries()? {
            self.poll()?;
            let entry = entry?;
            self.entries += 1;
            if self.entries > self.limits.entries {
                return Err(Failure::Limit);
            }
            let name = entry.file_name();
            let path = relative.join(&name);
            let metadata = source.symlink_metadata(&name)?;
            let mut record = Entry {
                path: path.as_os_str().as_bytes(),
                kind: "directory",
                mode: metadata.mode(),
                bytes: 0,
                digest: None,
                target: None,
            };
            if metadata.is_dir() {
                let child = source.open_dir_nofollow(&name)?;
                if !unchanged(&metadata, &child.dir_metadata()?) {
                    return Err(Failure::Changed);
                }
                let output = mkdir(destination, Path::new(&name))?;
                self.walk(&child, &output, &path, depth + 1)?;
                self.record(record)?;
            } else if metadata.is_file() {
                let mut input = source.open_with(&name, options().read(true))?;
                if !input.metadata()?.is_file() || !unchanged(&metadata, &input.metadata()?) {
                    return Err(Failure::Changed);
                }
                let mut output = create(destination, Path::new(&name))?;
                let mut digest = Sha256::new();
                let mut bytes = 0;
                let mut buffer = [0; 64 * 1024];
                loop {
                    self.poll()?;
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    self.charge(count as u64)?;
                    bytes += count as u64;
                    output.write_all(&buffer[..count])?;
                    digest.update(&buffer[..count]);
                }
                if bytes != metadata.len() || !unchanged(&metadata, &input.metadata()?) {
                    return Err(Failure::Changed);
                }
                output.sync_all()?;
                record.kind = "file";
                record.bytes = bytes;
                record.digest = Some(digest.finalize().into());
                self.record(record)?;
            } else if metadata.file_type().is_symlink() {
                let target = source.read_link_contents(&name)?;
                let target_bytes = target.as_os_str().as_bytes();
                self.charge(target_bytes.len() as u64)?;
                destination.symlink_contents(&target, &name)?;
                record.kind = "symlink";
                record.bytes = target_bytes.len() as u64;
                record.digest = Some(Sha256::digest(target_bytes).into());
                record.target = Some(target_bytes);
                self.record(record)?;
            } else {
                return Err(Failure::Unsupported);
            }
            if !unchanged(&metadata, &source.symlink_metadata(&name)?) {
                return Err(Failure::Changed);
            }
        }
        if !unchanged(&before, &source.dir_metadata()?) {
            return Err(Failure::Changed);
        }
        destination.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    }
}
pub(super) fn copy(source: &Dir, stage: &Dir, limits: Limits) -> Result<Reply> {
    let tree = mkdir(stage, Path::new("tree"))?;
    let manifest = create(stage, Path::new("manifest.jsonl"))?;
    let mut copy = Copy {
        limits,
        deadline: Instant::now() + Duration::from_millis(limits.timeout_ms),
        bytes: 0,
        entries: 1,
        manifest,
        hash: Sha256::new(),
    };
    copy.record(Entry {
        path: b"",
        kind: "directory",
        mode: source.dir_metadata()?.mode(),
        bytes: 0,
        digest: None,
        target: None,
    })?;
    copy.walk(source, &tree, Path::new(""), 0)?;
    copy.poll()?;
    copy.manifest.sync_all()?;
    stage.try_clone()?.into_std_file().sync_all()?;
    Ok(Reply::Copied {
        bytes: copy.bytes,
        entries: copy.entries,
        manifest_digest: copy.hash.finalize().into(),
    })
}
