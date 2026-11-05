//! Store errors mapped to the neutral error classes and causes of the identity
//! specification. Class and cause strings are provisional wire names. No
//! variant carries presented credential text.
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// A malformed or out-of-range input; `field` names it.
    InvalidRequest {
        field: &'static str,
    },
    /// The presented credential is missing, malformed, unknown, revoked or
    /// expired. The variants are deliberately indistinguishable.
    Unauthenticated,
    NotFound {
        what: &'static str,
    },
    /// The pairing code is malformed, unknown, expired, out of attempts,
    /// already claimed by another client, or presented by the wrong caller.
    PairingInvalid,
    /// The identity database could not be read or written, or holds data this
    /// version does not understand.
    Unavailable {
        detail: String,
    },
}

impl StoreError {
    pub(crate) fn unavailable(detail: impl Into<String>) -> Self {
        Self::Unavailable {
            detail: detail.into(),
        }
    }

    /// The neutral error class string.
    pub fn class(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } | Self::PairingInvalid => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::NotFound { .. } => "not_found",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    /// The cause code, when the specification defines one.
    pub fn cause(&self) -> Option<&'static str> {
        matches!(self, Self::PairingInvalid).then_some("pairing_invalid")
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { field } => write!(formatter, "invalid_request: {field}"),
            Self::Unauthenticated => formatter.write_str("unauthenticated"),
            Self::NotFound { what } => write!(formatter, "not_found: {what}"),
            Self::PairingInvalid => formatter.write_str("invalid_request: pairing_invalid"),
            Self::Unavailable { detail } => {
                write!(formatter, "unavailable: identity store: {detail}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

pub(crate) trait Storage<T> {
    fn storage(self) -> Result<T, StoreError>;
}

impl<T> Storage<T> for rusqlite::Result<T> {
    fn storage(self) -> Result<T, StoreError> {
        self.map_err(|error| StoreError::unavailable(error.to_string()))
    }
}
