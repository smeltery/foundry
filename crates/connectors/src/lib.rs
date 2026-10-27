//! Typed catalog validation, in-memory auth, and bounded MCP HTTP transport. No credential storage.

mod catalog;
mod client;
mod http;
mod network;
mod request;
mod sse;
mod validation;

pub use catalog::{
    AdapterState, Auth, Catalog, Connector, Endpoint, Icon, Placeholder, Server, Skill, Transport,
};
pub use client::{MCP_PROTOCOL_VERSION, McpHttpClient, ToolDefinition, ToolPage, ToolResult};
pub use http::{HttpLimits, McpError};
pub use network::NetworkPolicy;
pub use request::{AuthKind, ConfigurationStatus, Credentials, PreparedRequest, Secret};
use std::fmt;

/// Diagnostics never include catalog values, credentials, URLs or parser input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorError {
    InvalidJson,
    InvalidCatalog,
    InvalidUrl,
    UnsafeHost,
    InvalidHeaders,
    InvalidTemplate,
    MissingCredential,
    WrongCredentialKind,
    InvalidCredential,
    UnsupportedAdapter,
    UnsupportedQueryAuth,
    InsecureTransport,
    FixtureOnly,
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connector configuration rejected ({self:?})")
    }
}

impl std::error::Error for ConnectorError {}
