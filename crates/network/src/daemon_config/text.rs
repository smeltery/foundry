use super::{ConfigError, inputs};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A startup secret, supplied inline or by an explicit bounded file reference.
/// No Debug/Serialize; referenced paths are never included in errors.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SecretText {
    Inline(String),
    File(TextFile),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextFile {
    pub path: PathBuf,
}
impl SecretText {
    pub(super) fn inline(&self) -> Option<&str> {
        match self {
            Self::Inline(text) => Some(text),
            Self::File(_) => None,
        }
    }
    pub(super) fn validate(&self, limit: usize) -> Result<(), ConfigError> {
        match self {
            Self::Inline(text) if text.len() > limit => Err(ConfigError::Invalid),
            Self::File(file) if file.path.as_os_str().is_empty() => Err(ConfigError::Invalid),
            _ => Ok(()),
        }
    }
    pub(super) fn resolve(&self, base: &Path, limit: usize) -> Result<String, ConfigError> {
        self.validate(limit)?;
        match self {
            Self::Inline(text) => Ok(text.clone()),
            Self::File(file) => String::from_utf8(inputs::read(base, &file.path, limit as u64)?)
                .map_err(|_| ConfigError::Invalid),
        }
    }
}
