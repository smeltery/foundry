//! Key validation and account checks. Both ask a [`CredentialValidator`];
//! the only implementation here is the deterministic, offline fixture.
use super::{CredentialError, Secret};
use crate::catalog::{AuthKind, ProviderCatalog, ProviderDescriptor, ProviderId};

/// `Verified` alone means the key works; `Unverified` never means invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationOutcome {
    Verified,
    Rejected,
    Unverified(CheckCause),
}

impl ValidationOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Rejected => "rejected",
            Self::Unverified(_) => "unverified",
        }
    }

    pub fn is_valid(self) -> bool {
        self == Self::Verified
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationStatus {
    Accepted,
    Rejected,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityStatus {
    Available,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceBasis {
    ProviderMetadata,
    SavedCredential,
    KeyBudget,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckCause {
    MetadataAccepted,
    MetadataRejected,
    KeyBudgetRemaining,
    NotConnected,
    StorageUnavailable,
    ProviderUnavailable,
}

impl CheckCause {
    pub fn name(self) -> &'static str {
        match self {
            Self::MetadataAccepted => "metadata_accepted",
            Self::MetadataRejected => "metadata_rejected",
            Self::KeyBudgetRemaining => "key_budget_remaining",
            Self::NotConnected => "not_connected",
            Self::StorageUnavailable => "storage_unavailable",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence<S> {
    pub status: S,
    pub basis: EvidenceBasis,
    pub cause: CheckCause,
}

/// What the provider's metadata says about a key, without any model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderEvidence {
    pub authentication: Evidence<AuthenticationStatus>,
    pub capacity: Evidence<CapacityStatus>,
}

impl ProviderEvidence {
    /// Nothing could be learned: unverified authentication, unknown capacity.
    pub fn unverified(basis: EvidenceBasis, cause: CheckCause) -> Self {
        Self {
            authentication: Evidence {
                status: AuthenticationStatus::Unverified,
                basis,
                cause,
            },
            capacity: Evidence {
                status: CapacityStatus::Unknown,
                basis: EvidenceBasis::Unknown,
                cause,
            },
        }
    }
}

/// The result of `accounts.check`. Inference is never tested and the verdict
/// is never persisted; accepted authentication does not establish model
/// access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCheck {
    /// `None` when the credential file could not be read.
    pub connected: Option<bool>,
    pub evidence: ProviderEvidence,
    pub observed_at_ms: u64,
}

impl AccountCheck {
    pub const INFERENCE: &'static str = "not_tested";
}

/// Asks a provider about a key using metadata only.
pub trait CredentialValidator {
    fn validate(&self, provider: &ProviderDescriptor, secret: &Secret) -> ValidationOutcome;
    fn check(&self, provider: &ProviderDescriptor, secret: &Secret) -> ProviderEvidence;
}

/// Deterministic offline validator: no network, no clock, no randomness. Only
/// [`FixtureValidator::VALID_KEY`] is accepted; when `reachable` is false every
/// answer is unverified with `provider_unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixtureValidator {
    pub reachable: bool,
}

impl FixtureValidator {
    pub const VALID_KEY: &'static str = "fixture-valid";

    fn accepts(secret: &Secret) -> bool {
        // Compared through `Secret`'s constant-time equality.
        Secret::new(Self::VALID_KEY.to_owned()).is_ok_and(|valid| valid == *secret)
    }
}

impl CredentialValidator for FixtureValidator {
    fn validate(&self, _provider: &ProviderDescriptor, secret: &Secret) -> ValidationOutcome {
        match (self.reachable, Self::accepts(secret)) {
            (false, _) => ValidationOutcome::Unverified(CheckCause::ProviderUnavailable),
            (true, true) => ValidationOutcome::Verified,
            (true, false) => ValidationOutcome::Rejected,
        }
    }

    fn check(&self, provider: &ProviderDescriptor, secret: &Secret) -> ProviderEvidence {
        let metadata = EvidenceBasis::ProviderMetadata;
        match self.validate(provider, secret) {
            ValidationOutcome::Verified => ProviderEvidence {
                authentication: Evidence {
                    status: AuthenticationStatus::Accepted,
                    basis: metadata,
                    cause: CheckCause::MetadataAccepted,
                },
                capacity: Evidence {
                    status: CapacityStatus::Available,
                    basis: EvidenceBasis::KeyBudget,
                    cause: CheckCause::KeyBudgetRemaining,
                },
            },
            ValidationOutcome::Rejected => {
                let mut evidence =
                    ProviderEvidence::unverified(metadata, CheckCause::MetadataRejected);
                evidence.authentication.status = AuthenticationStatus::Rejected;
                evidence
            }
            ValidationOutcome::Unverified(cause) => ProviderEvidence::unverified(metadata, cause),
        }
    }
}

/// Validates a key against the provider without saving it.
pub fn validate(
    catalog: &ProviderCatalog,
    validator: &impl CredentialValidator,
    provider_id: &ProviderId,
    secret: &Secret,
) -> Result<ValidationOutcome, CredentialError> {
    let provider = catalog
        .provider(provider_id)
        .ok_or(CredentialError::NotFound { what: "provider" })?;
    if !provider.accepts(AuthKind::ApiKey) {
        return Err(CredentialError::UnsupportedProvider);
    }
    Ok(validator.validate(provider, secret))
}
