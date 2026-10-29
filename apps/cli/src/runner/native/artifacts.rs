use super::*;
use std::{
    fs::{DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) struct Files {
    directory: PathBuf,
    pub paths: foundry_service::profile_host::Artifacts,
}
impl Files {
    pub fn create(parent: &Path) -> Result<Self, CommandError> {
        let directory = parent.join(format!("capture-{}", uuid::Uuid::new_v4()));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| failure("capture directory unavailable"))?;
        Ok(Self {
            paths: foundry_service::profile_host::Artifacts {
                plan_path: directory.join("plan.json"),
                producer_path: directory.join("producer.json"),
            },
            directory,
        })
    }
    pub fn write_plan(&self, bytes: &[u8]) -> Result<(), CommandError> {
        if bytes.is_empty() || bytes.len() > 1048576 {
            return Err(failure("invalid capture plan"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.paths.plan_path)
            .map_err(|_| failure("capture plan unavailable"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| failure("capture plan write failed"))
    }
    pub fn remove(self) -> Result<(), CommandError> {
        std::fs::remove_dir_all(&self.directory)
            .map_err(|_| failure("capture artifacts cleanup failed"))
    }
}
