use super::{AuthKind, ProviderDescriptor, ProviderId};
use std::collections::BTreeSet;

/// Reports which credentials are configured, without exposing any value. The
/// credential store implements this later; the catalog only asks the question.
pub trait CredentialAvailability {
    fn has_credential(&self, provider_id: &ProviderId, kind: AuthKind) -> bool;
}

/// A fixed set of configured provider and credential kind pairs, for callers that
/// already hold the answer and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredCredentials(BTreeSet<(ProviderId, AuthKind)>);

impl ConfiguredCredentials {
    pub fn new(pairs: impl IntoIterator<Item = (ProviderId, AuthKind)>) -> Self {
        Self(pairs.into_iter().collect())
    }
}

impl CredentialAvailability for ConfiguredCredentials {
    fn has_credential(&self, provider_id: &ProviderId, kind: AuthKind) -> bool {
        self.0.contains(&(provider_id.clone(), kind))
    }
}

/// A provider can serve now when it needs no credential, or when a credential of
/// a kind it accepts is configured. A credential of another kind does not count.
pub(super) fn provider_available(
    provider: &ProviderDescriptor,
    credentials: &impl CredentialAvailability,
) -> bool {
    provider.auth_kinds.iter().any(|kind| {
        *kind == AuthKind::None || credentials.has_credential(&provider.provider_id, *kind)
    })
}
