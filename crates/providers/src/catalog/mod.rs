//! Provider catalog and model capability registry. It holds secret-free
//! descriptors, answers availability from injected credential state, and admits
//! model selections with a recorded decision. It stores no credentials, makes no
//! network calls and is not yet wired into the runtime.

mod admission;
mod availability;
mod descriptor;
mod effort;
mod error;
mod model;

pub use admission::{
    Admission, AttemptDecision, AutoRequest, AutoResolution, EffortDecision, EffortSource,
    ModelChoice, ModelRequest, ResolutionRule, SelectionRequest, SessionSelection,
};
pub use availability::{ConfiguredCredentials, CredentialAvailability};
pub use descriptor::{
    AccountMode, AuthKind, ConfigField, EndpointFormat, LoginMethod, LoginUnavailable, OauthLogin,
    ProviderDescriptor, ProviderId, ProviderKind,
};
pub use effort::Effort;
pub use error::{AdmissionError, AutoCause, CatalogError};
pub use model::{AUTO, ModelDescriptor, ModelEntry, ModelId, ModelRef};

use std::collections::BTreeMap;

/// An immutable, validated catalog revision. A changed catalog is a new value
/// with a new revision, so a recorded resolution names exactly what it used.
///
/// The catalog does not derive or check its revision. The caller must never
/// reuse a revision number for different providers, models or default;
/// otherwise a recorded resolution cannot identify the catalog it used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalog {
    revision: u64,
    providers: BTreeMap<ProviderId, ProviderDescriptor>,
    models: BTreeMap<ModelRef, ModelDescriptor>,
    workspace_default: Option<ModelRef>,
}

impl ProviderCatalog {
    pub fn new(
        revision: u64,
        providers: Vec<ProviderDescriptor>,
        models: Vec<ModelDescriptor>,
        workspace_default: Option<ModelRef>,
    ) -> Result<Self, CatalogError> {
        let mut by_id = BTreeMap::new();
        for provider in providers {
            let provider = provider.normalize()?;
            if by_id
                .insert(provider.provider_id.clone(), provider)
                .is_some()
            {
                return Err(CatalogError::DuplicateProvider);
            }
        }
        let mut by_key = BTreeMap::new();
        for model in models {
            let provider = by_id
                .get(&model.provider_id)
                .ok_or(CatalogError::ModelProviderUnknown)?;
            model.validate(provider)?;
            if by_key.insert(model.key(), model).is_some() {
                return Err(CatalogError::DuplicateModel);
            }
        }
        if let Some(key) = &workspace_default {
            let model: &ModelDescriptor = by_key
                .get(key)
                .ok_or(CatalogError::UnknownWorkspaceDefault)?;
            if model.hidden {
                return Err(CatalogError::HiddenWorkspaceDefault);
            }
        }
        Ok(Self {
            revision,
            providers: by_id,
            models: by_key,
            workspace_default,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn workspace_default(&self) -> Option<&ModelRef> {
        self.workspace_default.as_ref()
    }

    /// Descriptors ordered by provider ID.
    pub fn providers(&self) -> impl Iterator<Item = &ProviderDescriptor> {
        self.providers.values()
    }

    pub fn provider(&self, provider_id: &ProviderId) -> Option<&ProviderDescriptor> {
        self.providers.get(provider_id)
    }

    pub fn provider_available(
        &self,
        provider_id: &ProviderId,
        credentials: &impl CredentialAvailability,
    ) -> Result<bool, AdmissionError> {
        let provider = self
            .provider(provider_id)
            .ok_or(AdmissionError::UnknownProvider)?;
        Ok(availability::provider_available(provider, credentials))
    }

    /// Listed models ordered by provider then model ID, each flagged with its
    /// current availability. Hidden models are never listed. The specified
    /// grouping (Foundry-configured providers before connected accounts) is
    /// pending the credential store, which owns that distinction.
    pub fn models(&self, credentials: &impl CredentialAvailability) -> Vec<ModelEntry<'_>> {
        self.models
            .values()
            .filter(|model| !model.hidden)
            .map(|model| ModelEntry {
                model,
                available: self.model_available(model, credentials),
            })
            .collect()
    }

    pub fn model(&self, key: &ModelRef) -> Option<&ModelDescriptor> {
        self.models.get(key)
    }

    fn model_available(
        &self,
        model: &ModelDescriptor,
        credentials: &impl CredentialAvailability,
    ) -> bool {
        // Construction guarantees every model's provider is registered.
        self.providers
            .get(&model.provider_id)
            .is_some_and(|provider| availability::provider_available(provider, credentials))
    }
}
