use super::descriptor::{is_label, is_unique};
use super::{CatalogError, Effort, ProviderDescriptor, ProviderId};
use std::fmt;

/// The request token that asks for the workspace default. It is never a model ID.
pub const AUTO: &str = "auto";

/// A concrete model identifier: 1 to 256 bytes, nonblank, without NUL or other
/// control characters, and never the reserved `auto` token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: &str) -> Result<Self, CatalogError> {
        if value.trim().is_empty()
            || value.len() > 256
            || value.chars().any(char::is_control)
            || value == AUTO
        {
            return Err(CatalogError::InvalidModelId);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A model's catalog key. Model IDs are unique per provider, not globally.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelRef {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
}

/// One registered model. A model that offers efforts names one of them as its
/// default; a model that offers none takes no effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDescriptor {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub label: String,
    pub efforts: Vec<Effort>,
    pub default_effort: Option<Effort>,
    /// Kept only so existing sessions can continue; never listed or newly chosen.
    pub hidden: bool,
}

impl ModelDescriptor {
    pub fn key(&self) -> ModelRef {
        ModelRef {
            provider_id: self.provider_id.clone(),
            model_id: self.model_id.clone(),
        }
    }

    pub fn offers(&self, effort: Effort) -> bool {
        self.efforts.contains(&effort)
    }

    pub(super) fn validate(&self, provider: &ProviderDescriptor) -> Result<(), CatalogError> {
        if !is_label(&self.label) {
            return Err(CatalogError::InvalidLabel);
        }
        if !is_unique(&self.efforts) {
            return Err(CatalogError::InvalidEfforts);
        }
        if !self.efforts.iter().all(|effort| provider.offers(*effort)) {
            return Err(CatalogError::ModelEffortOutsideProvider);
        }
        let default_valid = match self.default_effort {
            Some(effort) => self.offers(effort),
            None => self.efforts.is_empty(),
        };
        if !default_valid {
            return Err(CatalogError::InvalidDefaultEffort);
        }
        Ok(())
    }
}

/// A listed model with its availability under the injected credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelEntry<'a> {
    pub model: &'a ModelDescriptor,
    pub available: bool,
}
