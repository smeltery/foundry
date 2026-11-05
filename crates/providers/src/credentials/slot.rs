//! Account slots, their lifecycle states and who may change them.
use super::CredentialError;
use crate::catalog::{AuthKind, ProviderId};
use foundry_identity::authz::{PrincipalId, Role, WorkspaceId};

/// Names one account slot of a provider: 1 to 512 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountKey(String);

impl AccountKey {
    pub const MAX_CHARS: usize = 512;

    pub fn new(value: &str) -> Result<Self, CredentialError> {
        if (1..=Self::MAX_CHARS).contains(&value.chars().count()) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CredentialError::invalid("account_key"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A principal already admitted to `workspace` by the authorization core.
/// Every slot belongs to exactly one workspace and is invisible from others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub workspace: WorkspaceId,
    pub principal: PrincipalId,
    pub role: Role,
}

impl Caller {
    /// Viewers read slot state but never connect, replace, delete, check or
    /// use a credential.
    pub(crate) fn may_write(&self) -> bool {
        matches!(self.role, Role::Owner | Role::Admin | Role::Member)
    }

    /// Owners and administrators act on any slot; a member only on slots
    /// whose current credential they supplied.
    pub(crate) fn may_manage(&self, connected_by: &PrincipalId) -> bool {
        match self.role {
            Role::Owner | Role::Admin => true,
            Role::Member => *connected_by == self.principal,
            Role::Viewer => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialState {
    Configured,
    AuthFailed,
    ReconnectRequired,
}

impl CredentialState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::AuthFailed => "auth_failed",
            Self::ReconnectRequired => "reconnect_required",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [Self::Configured, Self::AuthFailed, Self::ReconnectRequired]
            .into_iter()
            .find(|state| state.name() == name)
    }

    /// The state after a provider-side event, or `None` when the table in the
    /// specification has no such transition. Configure, replace and reconnect
    /// always lead to `Configured`, and delete always to absent.
    pub fn after(self, event: CredentialEvent) -> Option<Self> {
        match (self, event) {
            (Self::Configured, CredentialEvent::ProviderRefused) => Some(Self::AuthFailed),
            (Self::Configured, CredentialEvent::TokenExpired) => Some(Self::ReconnectRequired),
            _ => None,
        }
    }
}

/// A provider-side event that moves a configured slot out of `Configured`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialEvent {
    ProviderRefused,
    TokenExpired,
}

impl CredentialEvent {
    /// The failure code a slot reports after this event.
    pub fn failure_code(self) -> &'static str {
        match self {
            Self::ProviderRefused => "provider_refused",
            Self::TokenExpired => "token_expired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub at_ms: u64,
}

/// What a read reveals about a slot. It has no secret field, not even masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSlot {
    pub provider_id: ProviderId,
    pub account_key: AccountKey,
    pub kind: AuthKind,
    pub state: CredentialState,
    /// Names of the configured non-secret fields, in name order.
    pub config_names: Vec<String>,
    /// Whoever supplied the current credential.
    pub connected_by: PrincipalId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub failure: Option<Failure>,
}

pub(crate) fn kind_name(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::ApiKey => "api_key",
        AuthKind::Oauth => "oauth",
        AuthKind::None => "none",
    }
}

pub(crate) fn kind_from_name(name: &str) -> Option<AuthKind> {
    [AuthKind::ApiKey, AuthKind::Oauth]
        .into_iter()
        .find(|kind| kind_name(*kind) == name)
}

pub(crate) fn failure_from_code(code: &str, at_ms: u64) -> Option<Failure> {
    [
        CredentialEvent::ProviderRefused,
        CredentialEvent::TokenExpired,
    ]
    .into_iter()
    .map(CredentialEvent::failure_code)
    .find(|known| *known == code)
    .map(|code| Failure { code, at_ms })
}
