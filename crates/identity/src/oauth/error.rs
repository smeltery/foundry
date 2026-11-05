//! Authorization server errors. Class strings follow the neutral OAuth error
//! vocabulary and are provisional wire names. No variant carries presented
//! code, token, verifier or client secret text.
use std::fmt;

use crate::store::StoreError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// A missing, malformed or unsupported parameter; `field` names it. A
    /// `plain` or absent PKCE method lands here.
    InvalidRequest { field: &'static str },
    /// The client is unknown, or its authentication is missing or wrong.
    InvalidClient,
    /// The code, device code or refresh token is unknown, expired, already
    /// used, revoked, issued to another client, or fails its redirect or
    /// verifier binding. The cases are deliberately indistinguishable.
    InvalidGrant,
    /// The client may not act on this token, as when it revokes a token
    /// issued to another client.
    UnauthorizedClient,
    /// A requested scope is malformed or not registered for the client.
    InvalidScope,
    /// The device grant is still waiting for the user.
    AuthorizationPending,
    /// The device grant was polled faster than its interval.
    SlowDown,
    /// The user denied the device grant.
    AccessDenied,
    /// The device grant expired before it was approved and exchanged.
    ExpiredToken,
    /// The random source or the fixture's state could not be used.
    Unavailable { detail: String },
}

impl OAuthError {
    /// The neutral error class string.
    pub fn class(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "invalid_request",
            Self::InvalidClient => "invalid_client",
            Self::InvalidGrant => "invalid_grant",
            Self::UnauthorizedClient => "unauthorized_client",
            Self::InvalidScope => "invalid_scope",
            Self::AuthorizationPending => "authorization_pending",
            Self::SlowDown => "slow_down",
            Self::AccessDenied => "access_denied",
            Self::ExpiredToken => "expired_token",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { field } => write!(formatter, "invalid_request: {field}"),
            Self::Unavailable { detail } => {
                write!(formatter, "unavailable: authorization server: {detail}")
            }
            other => formatter.write_str(other.class()),
        }
    }
}

impl std::error::Error for OAuthError {}

impl From<StoreError> for OAuthError {
    /// Only the random source reaches here; its message names no secret.
    fn from(error: StoreError) -> Self {
        Self::Unavailable {
            detail: error.to_string(),
        }
    }
}
