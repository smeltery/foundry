use std::fmt;

/// A catalog or descriptor that cannot be registered. Diagnostics never echo the
/// offending value, so a caller can log them without leaking configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    InvalidProviderId,
    InvalidModelId,
    InvalidLabel,
    DuplicateProvider,
    DuplicateModel,
    InvalidAuthKinds,
    InvalidConfigFields,
    InvalidOauthLogin,
    InvalidEndpointFormat,
    UnknownEffort,
    InvalidEfforts,
    ModelProviderUnknown,
    ModelEffortOutsideProvider,
    InvalidDefaultEffort,
    UnknownWorkspaceDefault,
    HiddenWorkspaceDefault,
}

impl CatalogError {
    /// Provisional neutral error class from the providers specification.
    pub fn class(self) -> &'static str {
        "invalid_request"
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "provider catalog rejected ({self:?})")
    }
}

impl std::error::Error for CatalogError {}

/// Why `auto` could not be resolved. Resolution never falls back to another model
/// or provider, so each cause is reported instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoCause {
    NoWorkspaceDefault,
    DefaultUnavailable,
    DefaultOutsideProvider,
}

/// A model selection refused at admission, before anything executes. Each
/// variant is distinct so callers and tests can tell the refusals apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    /// The request named an invalid provider or model identifier.
    InvalidRequest(CatalogError),
    UnknownProvider,
    UnknownModel,
    /// A hidden model is admitted only to continue a session already using it.
    HiddenModel,
    /// The model identifier is listed by several providers and none was named.
    AmbiguousModel,
    /// A follow-up named a provider other than the session model's provider
    /// without naming a model.
    ProviderMismatch,
    ModelUnavailable,
    EffortNotOffered,
    AutoUnresolvable(AutoCause),
}

impl AdmissionError {
    /// Provisional neutral error class from the providers specification.
    pub fn class(self) -> &'static str {
        match self {
            Self::InvalidRequest(_)
            | Self::AmbiguousModel
            | Self::ProviderMismatch
            | Self::EffortNotOffered => "invalid_request",
            Self::UnknownProvider | Self::UnknownModel | Self::HiddenModel => "not_found",
            Self::ModelUnavailable | Self::AutoUnresolvable(_) => "model_unavailable",
        }
    }
}

impl From<CatalogError> for AdmissionError {
    fn from(error: CatalogError) -> Self {
        Self::InvalidRequest(error)
    }
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "model selection refused ({self:?})")
    }
}

impl std::error::Error for AdmissionError {}
