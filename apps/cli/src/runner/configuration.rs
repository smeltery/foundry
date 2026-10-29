//! Explicit private configuration, loaded once without account discovery.
use crate::CommandError;
use foundry_contracts::adapter_selection::AdapterSelection;
use foundry_service::prepared_adapter::{AdapterCredential, AdapterPrivateInputs};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 65_536;

pub(super) struct LocalAdapterConfiguration {
    pub selection: AdapterSelection,
    pub private: AdapterPrivateInputs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    version: u32,
    selection: AdapterSelection,
    private: Private,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Private {
    credential: Credential,
    system_prompt: String,
    agent_directory: PathBuf,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Credential {
    None,
    ApiKey { provider: String, value: String },
}

pub(super) fn load(path: &Path) -> Result<LocalAdapterConfiguration, CommandError> {
    decode(&read_private(path)?)
}

#[cfg(unix)]
pub(super) fn read_private(path: &Path) -> Result<Vec<u8>, CommandError> {
    use std::{
        fs::OpenOptions,
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    if !path.is_absolute() {
        return Err(invalid());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
        || !(1..=MAX_BYTES).contains(&metadata.len())
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(not(unix))]
pub(super) fn read_private(_: &Path) -> Result<Vec<u8>, CommandError> {
    Err(CommandError::new(
        "unsupported",
        "Private adapter configuration requires a Unix platform",
    ))
}

fn decode(bytes: &[u8]) -> Result<LocalAdapterConfiguration, CommandError> {
    let config: Configuration = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if config.version != 1 || config.selection.validate().is_err() {
        return Err(invalid());
    }
    let private = config.private;
    let directory = private.agent_directory.to_str().ok_or_else(invalid)?;
    if !private.agent_directory.is_absolute()
        || directory.len() > 4096
        || directory.contains('\0')
        || private.system_prompt.len() > 16_384
    {
        return Err(invalid());
    }
    let credential = match private.credential {
        Credential::None => AdapterCredential::None,
        Credential::ApiKey { provider, value }
            if provider == config.selection.provider
                && !value.is_empty()
                && value.len() <= 16_384 =>
        {
            AdapterCredential::ApiKey { provider, value }
        }
        Credential::ApiKey { .. } => return Err(invalid()),
    };
    Ok(LocalAdapterConfiguration {
        selection: config.selection,
        private: AdapterPrivateInputs {
            credential,
            system_prompt: private.system_prompt,
            agent_directory: private.agent_directory,
        },
    })
}

fn invalid() -> CommandError {
    CommandError::new(
        "configuration",
        "Private adapter configuration is unavailable or invalid",
    )
}

#[cfg(all(test, unix))]
#[path = "configuration_tests.rs"]
mod tests;
