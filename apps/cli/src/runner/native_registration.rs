//! Explicit installation identity; private configuration cannot choose host code.
use super::configuration::read_private;
use crate::CommandError;
use foundry_contracts::context::{ContextDigest, ContextFormat, ContextId};
use foundry_service::profile_host::{Registration, RegistrationOptions, StoredProfile};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    version: u32,
    executable: PathBuf,
    stored: StoredProfile,
    wire: ContextFormat,
    validator_id: ContextId,
    validator_version: ContextId,
    observation_profile_digest: ContextDigest,
    registration_digest: ContextDigest,
    api: ContextId,
}

#[derive(Serialize)]
struct ProfileConfiguration<'a> {
    version: u32,
    api: &'a str,
    system_prompt: &'a str,
}

pub(super) fn load(path: &Path, system_prompt: &str) -> Result<Registration, CommandError> {
    let bytes = read_private(path).map_err(|_| invalid())?;
    Registration::new(options(&bytes, system_prompt)?).map_err(|_| invalid())
}

fn options(bytes: &[u8], system_prompt: &str) -> Result<RegistrationOptions, CommandError> {
    let installation: Installation = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if installation.version != 1 || system_prompt.is_empty() || system_prompt.len() > 16_384 {
        return Err(invalid());
    }
    let configuration = serde_json::to_vec(&ProfileConfiguration {
        version: 1,
        api: installation.api.as_str(),
        system_prompt,
    })
    .map_err(|_| invalid())?;
    Ok(RegistrationOptions {
        executable: installation.executable,
        stored: installation.stored,
        wire: installation.wire,
        validator_id: installation.validator_id,
        validator_version: installation.validator_version,
        observation_profile_digest: installation.observation_profile_digest,
        registration_digest: installation.registration_digest,
        configuration,
    })
}

fn invalid() -> CommandError {
    CommandError::new(
        "configuration",
        "Native profile registration is unavailable or invalid",
    )
}

#[cfg(all(test, unix))]
#[path = "native_registration_tests.rs"]
mod tests;
