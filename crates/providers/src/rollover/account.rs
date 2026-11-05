//! Accounts offered as rollover targets and why one was passed over.
use crate::catalog::ProviderId;
use crate::credentials::{AccountKey, CredentialState};
use foundry_identity::authz::WorkspaceId;

/// One connected account as the credential store reports it. The caller may
/// offer every account it knows; the policy itself narrows them to the
/// session's workspace and provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCandidate {
    pub workspace: WorkspaceId,
    pub provider_id: ProviderId,
    pub account_key: AccountKey,
    pub state: CredentialState,
    /// False when the account cannot serve now, for example when its storage
    /// is unreadable or a capacity check reported it blocked.
    pub available: bool,
}

/// Why an account of the session's workspace and provider was not chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipCause {
    /// An earlier attempt of this chain already ran on it.
    AlreadyTried,
    AuthFailed,
    ReconnectRequired,
    Unavailable,
}

impl SkipCause {
    pub fn name(self) -> &'static str {
        match self {
            Self::AlreadyTried => "already_tried",
            Self::AuthFailed => "auth_failed",
            Self::ReconnectRequired => "reconnect_required",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedAccount {
    pub account_key: AccountKey,
    pub cause: SkipCause,
}

impl AccountCandidate {
    /// `None` when the account can take the next attempt. A tried account is
    /// reported as tried whatever its state, so chains stay bounded.
    pub(super) fn skip_cause(&self, tried: bool) -> Option<SkipCause> {
        if tried {
            return Some(SkipCause::AlreadyTried);
        }
        match self.state {
            CredentialState::AuthFailed => Some(SkipCause::AuthFailed),
            CredentialState::ReconnectRequired => Some(SkipCause::ReconnectRequired),
            CredentialState::Configured if !self.available => Some(SkipCause::Unavailable),
            CredentialState::Configured => None,
        }
    }
}
