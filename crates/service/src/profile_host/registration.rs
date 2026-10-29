use super::{Error, Result};
use foundry_contracts::context::{ContextDigest, ContextFormat, ContextId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredProfile {
    pub format_id: String,
    pub format_version: u32,
    pub profile_id: String,
    pub binding_id: String,
    pub binding_version: String,
}

/// Explicit trusted installation data; never constructed from a host response.
pub struct RegistrationOptions {
    pub executable: PathBuf,
    pub stored: StoredProfile,
    pub wire: ContextFormat,
    pub validator_id: ContextId,
    pub validator_version: ContextId,
    pub observation_profile_digest: ContextDigest,
    pub registration_digest: ContextDigest,
    pub configuration: Vec<u8>,
}

/// Carries private configuration without Debug or serialization.
pub struct Registration {
    pub(crate) options: RegistrationOptions,
}
impl Registration {
    pub fn new(mut options: RegistrationOptions) -> Result<Self> {
        if !options.executable.is_absolute()
            || options.configuration.is_empty()
            || options.configuration.len() > 65536
            || options.stored.format_version == 0
        {
            return Err(Error::Policy);
        }
        for value in [
            &options.stored.format_id,
            &options.stored.profile_id,
            &options.stored.binding_id,
            &options.stored.binding_version,
        ] {
            ContextId::new(value).map_err(|_| Error::Policy)?;
        }
        options.executable = options
            .executable
            .canonicalize()
            .map_err(|_| Error::Policy)?;
        if !options.executable.is_file() {
            return Err(Error::Policy);
        }
        Ok(Self { options })
    }
}
