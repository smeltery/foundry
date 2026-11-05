use crate::catalog::ProviderId;
use std::fmt;

/// The setting or request field a refusal is about. It names the field only,
/// never its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidField {
    Descriptor,
    BaseUrl,
    Models,
    Timeout,
    Key,
    Messages,
    MaxOutputTokens,
}

/// Why an endpoint call failed. Names are provisional wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    Dns,
    Connect,
    Timeout,
    Tls,
    /// A status other than 200, including a redirect, which is never followed.
    Http(u16),
    /// A 200 reply that is oversized, encoded, not JSON or lacks the format's
    /// text.
    Protocol,
}

impl FailureCategory {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Http(_) => "http",
            Self::Protocol => "protocol",
        }
    }
}

/// A refused or failed endpoint operation. Neither `Display` nor `Debug` ever
/// contains the endpoint URL, a key, a request or a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    InvalidRequest(InvalidField),
    /// The request named a model outside the endpoint's explicit list.
    UnknownModel,
    /// A public endpoint without HTTPS.
    InsecureTransport,
    /// An address class the endpoint did not opt into, a link-local or reserved
    /// address, or DNS answers that span classes.
    NotPermitted,
    Failure {
        provider_id: ProviderId,
        category: FailureCategory,
    },
}

impl EndpointError {
    /// Provisional neutral error class from the providers specification.
    pub fn class(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::UnknownModel => "not_found",
            Self::InsecureTransport => "insecure_transport",
            Self::NotPermitted => "endpoint_not_permitted",
            Self::Failure { .. } => "endpoint_failure",
        }
    }
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(field) => write!(f, "endpoint request rejected ({field:?})"),
            Self::UnknownModel => f.write_str("model is not listed for this endpoint"),
            Self::InsecureTransport => f.write_str("public endpoint requires HTTPS"),
            Self::NotPermitted => f.write_str("endpoint address is not permitted"),
            Self::Failure {
                provider_id,
                category,
            } => {
                write!(f, "{provider_id} endpoint failure: {}", category.name())?;
                match category {
                    FailureCategory::Http(status) => write!(f, " {status}"),
                    _ => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for EndpointError {}
