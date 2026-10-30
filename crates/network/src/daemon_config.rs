//! Explicit foreground-daemon configuration. No global profile or hosted defaults.
use crate::session_registry::FolderPolicy;
use serde::Deserialize;
use std::{
    fmt,
    io::Read,
    path::{Path, PathBuf},
};

mod inputs;
mod network;
mod resolved;
mod startup;
mod text;
pub use network::{
    CodebookConfig, NetworkConfig, NetworkMode, SeedConfig, TransportConfig, TrustConfig,
};
pub use resolved::ResolvedNetwork;
pub use startup::{ResolvedStartup, StartupMode};
pub use text::{SecretText, TextFile};

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    pub state_directory: PathBuf,
    pub socket_directory: PathBuf,
    pub person: String,
    pub machine: String,
    pub network: NetworkMode,
    pub folders: Vec<FolderRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRule {
    pub path: PathBuf,
    pub allow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    Invalid,
    UnsafeFile,
    Storage,
    Unsupported,
}
impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid daemon configuration",
            Self::UnsafeFile => "daemon configuration file protection failed",
            Self::Storage => "daemon configuration could not be read",
            Self::Unsupported => "daemon configuration unsupported on this platform",
        })
    }
}
impl std::error::Error for ConfigError {}

impl DaemonConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.state_directory.is_absolute() || !self.socket_directory.is_absolute() {
            return Err(ConfigError::Invalid);
        }
        for value in [&self.person, &self.machine] {
            if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return Err(ConfigError::Invalid);
            }
        }
        self.folder_policy()?;
        if let NetworkMode::Enabled(network) = &self.network {
            network.validate()?;
        }
        Ok(())
    }

    pub fn folder_policy(&self) -> Result<FolderPolicy, ConfigError> {
        FolderPolicy::new(
            self.folders
                .iter()
                .map(|r| (r.path.clone(), r.allow))
                .collect(),
        )
        .map_err(|_| ConfigError::Invalid)
    }

    /// Rejects writable-by-others, foreign-owned and linked configuration files.
    /// Parsing errors never include input values in diagnostics.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        #[cfg(unix)]
        {
            use std::{
                fs::OpenOptions,
                os::unix::fs::{MetadataExt, OpenOptionsExt},
            };
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(|_| ConfigError::Storage)?;
            let metadata = file.metadata().map_err(|_| ConfigError::Storage)?;
            // geteuid reads the effective account identity without pointer arguments.
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o022 != 0
            {
                return Err(ConfigError::UnsafeFile);
            }
            if metadata.len() > MAX_CONFIG_BYTES {
                return Err(ConfigError::Invalid);
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(MAX_CONFIG_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ConfigError::Storage)?;
            if bytes.len() as u64 > MAX_CONFIG_BYTES {
                return Err(ConfigError::Invalid);
            }
            let config: Self = serde_json::from_slice(&bytes).map_err(|_| ConfigError::Invalid)?;
            config.validate()?;
            Ok(config)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(ConfigError::Unsupported)
        }
    }
}
