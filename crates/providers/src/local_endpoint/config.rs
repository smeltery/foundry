use super::{EndpointError, InvalidField, OptIn};
use crate::catalog::ModelId;
use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;
use url::Url;

/// Longest base URL an endpoint accepts.
const MAX_URL_BYTES: usize = 2048;
/// Most model IDs one endpoint lists.
const MAX_MODELS: usize = 256;
/// Upper bound on any configured call timeout.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(300);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A write-only endpoint key. It is sent as a sensitive header and never
/// appears in `Debug` output.
#[derive(Clone)]
pub struct EndpointKey(String);

impl EndpointKey {
    /// Visible ASCII only, at most 4096 bytes, so it is a valid header value.
    pub fn new(value: String) -> Result<Self, EndpointError> {
        if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(EndpointError::InvalidRequest(InvalidField::Key));
        }
        Ok(Self(value))
    }

    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EndpointKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EndpointKey([redacted])")
    }
}

/// How to reach one custom endpoint. Model IDs are listed explicitly; nothing
/// is discovered from the endpoint.
#[derive(Clone)]
pub struct EndpointSettings {
    pub base_url: String,
    pub models: Vec<ModelId>,
    pub opt_in: Vec<OptIn>,
    /// Bounds the whole call: name lookup, connection, request and reply.
    pub timeout: Duration,
    pub key: Option<EndpointKey>,
}

impl fmt::Debug for EndpointSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointSettings")
            .field("base_url", &"[redacted]")
            .field("models", &self.models)
            .field("opt_in", &self.opt_in)
            .field("timeout", &self.timeout)
            .field("key", &self.key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl EndpointSettings {
    pub(super) fn validate(&self) -> Result<Url, EndpointError> {
        let models: BTreeSet<_> = self.models.iter().collect();
        if models.is_empty() || models.len() != self.models.len() || models.len() > MAX_MODELS {
            return Err(EndpointError::InvalidRequest(InvalidField::Models));
        }
        if self.timeout.is_zero() || self.timeout > MAX_TIMEOUT {
            return Err(EndpointError::InvalidRequest(InvalidField::Timeout));
        }
        base_url(&self.base_url)
    }
}

/// An HTTP or HTTPS URL with a host and no user information, query, fragment,
/// whitespace, control characters or backslashes.
fn base_url(value: &str) -> Result<Url, EndpointError> {
    let invalid = EndpointError::InvalidRequest(InvalidField::BaseUrl);
    if value.len() > MAX_URL_BYTES
        || value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(invalid);
    }
    // Checked on the raw text too, since parsing can drop an empty user part.
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .ok_or_else(|| invalid.clone())?;
    let url = Url::parse(value).map_err(|_| invalid.clone())?;
    if !matches!(url.scheme(), "http" | "https")
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host().is_none()
        || url.port() == Some(0)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid);
    }
    Ok(url)
}
