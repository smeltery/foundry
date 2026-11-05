//! Organization errors mapped to the neutral error classes and causes of the
//! identity specification. Class and cause strings are provisional wire names.
use std::fmt;

use crate::authz::Denial;
use crate::store::StoreError;

/// Operation-rule causes. Authorization causes such as `owner_required` come
/// from the decision core inside [`OrgError::Denied`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgCause {
    LastOwner,
    NotMember,
    NameMismatch,
    KeyReused,
    WorkspaceRetired,
}

impl OrgCause {
    pub fn name(self) -> &'static str {
        match self {
            OrgCause::LastOwner => "last_owner",
            OrgCause::NotMember => "not_member",
            OrgCause::NameMismatch => "name_mismatch",
            OrgCause::KeyReused => "key_reused",
            OrgCause::WorkspaceRetired => "workspace_retired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgError {
    /// The decision core refused the request; nothing was read or written.
    Denied(Denial),
    /// A malformed or out-of-range input; `field` names it.
    InvalidRequest {
        field: &'static str,
        cause: Option<OrgCause>,
    },
    NotFound {
        what: &'static str,
        cause: Option<OrgCause>,
    },
    Conflict {
        cause: OrgCause,
    },
    /// The identity database could not be opened, read or written.
    Store(StoreError),
}

impl OrgError {
    pub(crate) fn invalid(field: &'static str) -> Self {
        Self::InvalidRequest { field, cause: None }
    }

    pub(crate) fn not_member() -> Self {
        Self::NotFound {
            what: "member",
            cause: Some(OrgCause::NotMember),
        }
    }

    pub(crate) fn unavailable(detail: impl Into<String>) -> Self {
        Self::Store(StoreError::Unavailable {
            detail: detail.into(),
        })
    }

    /// The neutral error class string.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Denied(denial) => denial.class.name(),
            Self::InvalidRequest { .. } => "invalid_request",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Store(error) => error.class(),
        }
    }

    /// The cause code, when the specification defines one.
    pub fn cause(&self) -> Option<&'static str> {
        match self {
            Self::Denied(denial) => denial.cause.map(|cause| cause.name()),
            Self::InvalidRequest { cause, .. } | Self::NotFound { cause, .. } => {
                cause.map(OrgCause::name)
            }
            Self::Conflict { cause } => Some(cause.name()),
            Self::Store(error) => error.cause(),
        }
    }
}

impl fmt::Display for OrgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Store(error) = self {
            return write!(formatter, "{error}");
        }
        formatter.write_str(self.class())?;
        match self {
            Self::InvalidRequest { field, .. } => write!(formatter, ": {field}")?,
            Self::NotFound { what, .. } => write!(formatter, ": {what}")?,
            Self::Denied(_) | Self::Conflict { .. } | Self::Store(_) => {}
        }
        match self.cause() {
            Some(cause) => write!(formatter, " ({cause})"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for OrgError {}

impl From<StoreError> for OrgError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<Denial> for OrgError {
    fn from(denial: Denial) -> Self {
        Self::Denied(denial)
    }
}

pub(crate) trait Storage<T> {
    fn storage(self) -> Result<T, OrgError>;
}

impl<T> Storage<T> for rusqlite::Result<T> {
    fn storage(self) -> Result<T, OrgError> {
        self.map_err(|error| OrgError::unavailable(error.to_string()))
    }
}
