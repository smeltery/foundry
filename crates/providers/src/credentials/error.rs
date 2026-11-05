use std::fmt;

/// A refused or failed credential operation. Every field is a static label, so
/// neither `Debug` nor `Display` can carry a key, token, config value or
/// storage message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    /// A malformed or out-of-range input; `field` names it, never its value.
    InvalidRequest {
        field: &'static str,
    },
    /// The caller's role does not permit the operation on this slot.
    Forbidden,
    NotFound {
        what: &'static str,
    },
    /// The operation does not apply to the slot's current state.
    Conflict {
        what: &'static str,
    },
    /// The provider does not accept the credential kind the operation needs.
    UnsupportedProvider,
    /// The credential file could not be read or written, or holds data this
    /// version does not understand.
    Unavailable {
        detail: &'static str,
    },
}

impl CredentialError {
    pub(crate) fn invalid(field: &'static str) -> Self {
        Self::InvalidRequest { field }
    }

    pub(crate) fn unavailable(detail: &'static str) -> Self {
        Self::Unavailable { detail }
    }

    /// Provisional neutral error class from the providers specification.
    pub fn class(self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "invalid_request",
            Self::Forbidden => "forbidden",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::UnsupportedProvider => "unsupported_provider",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "provider credential {} ({self:?})", self.class())
    }
}

impl std::error::Error for CredentialError {}
