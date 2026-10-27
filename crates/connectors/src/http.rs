use crate::{NetworkPolicy, PreparedRequest};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustls_platform_verifier::BuilderVerifierExt;
use std::{fmt, net::SocketAddr, time::Duration};
use tokio::time::{Instant, timeout_at};
use url::{Host, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpError {
    Policy,
    InvalidLimits,
    Timeout,
    Network,
    InvalidHeaders,
    TooLarge,
    Redirect,
    HttpStatus(u16),
    SessionExpired,
    Protocol,
    UnsupportedVersion,
    UnsupportedServerMessage,
    UnsupportedContentType,
    ToolsUnavailable,
    NotInitialized,
    AlreadyInitialized,
    RemoteRpc(i64),
}
impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP operation failed ({self:?})")
    }
}
impl std::error::Error for McpError {}

#[derive(Debug, Clone, Copy)]
pub struct HttpLimits {
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}
impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 1024 * 1024,
        }
    }
}

pub(crate) struct HttpTransport {
    request: PreparedRequest,
    policy: NetworkPolicy,
    pub limits: HttpLimits,
    #[cfg(test)]
    tls_override: Option<rustls::ClientConfig>,
}
impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpTransport([redacted])")
    }
}

pub(crate) struct HttpReply {
    pub response: reqwest::Response,
    pub event_stream: bool,
    pub session: Option<String>,
}

impl HttpTransport {
    pub fn new(
        request: PreparedRequest,
        policy: NetworkPolicy,
        limits: HttpLimits,
    ) -> Result<Self, McpError> {
        policy.url(request.url()).map_err(|_| McpError::Policy)?;
        if limits.timeout.is_zero()
            || limits.timeout > Duration::from_secs(300)
            || !(1..=16 * 1024 * 1024).contains(&limits.max_request_bytes)
            || !(1..=16 * 1024 * 1024).contains(&limits.max_response_bytes)
        {
            return Err(McpError::InvalidLimits);
        }
        if request.headers().keys().any(|key| {
            [
                "accept",
                "content-type",
                "mcp-session-id",
                "mcp-protocol-version",
            ]
            .contains(&key.as_str())
        }) {
            return Err(McpError::InvalidHeaders);
        }
        Ok(Self {
            request,
            policy,
            limits,
            #[cfg(test)]
            tls_override: None,
        })
    }

    async fn addresses(&self, url: &Url) -> Result<Vec<SocketAddr>, McpError> {
        let port = url.port_or_known_default().ok_or(McpError::Policy)?;
        let peers = match url.host().ok_or(McpError::Policy)? {
            Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
            Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
            Host::Domain(host) => tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| McpError::Network)?
                .take(65)
                .collect(),
        };
        self.policy.checked_peers(peers)
    }

    /// Build a fresh client pinned to vetted addresses for each POST. No second DNS lookup.
    pub async fn post(
        &self,
        body: Vec<u8>,
        session: Option<&str>,
        negotiated: bool,
        notification: bool,
    ) -> Result<HttpReply, McpError> {
        if body.len() > self.limits.max_request_bytes {
            return Err(McpError::TooLarge);
        }
        let url = self
            .policy
            .url(self.request.url())
            .map_err(|_| McpError::Policy)?;
        let peers = self.addresses(&url).await?;
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| McpError::Network)?
        .with_platform_verifier()
        .map_err(|_| McpError::Network)?
        .with_no_client_auth();
        #[cfg(test)]
        let tls = self.tls_override.clone().unwrap_or(tls);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .http1_only()
            .http1_max_headers(64)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .tls_sslkeylogfile(false)
            .tls_backend_preconfigured(tls)
            .https_only(!self.policy.is_local())
            .timeout(self.limits.timeout)
            .connect_timeout(self.limits.timeout)
            .read_timeout(self.limits.timeout)
            .resolve_to_addrs(url.host_str().ok_or(McpError::Policy)?, &peers)
            .build()
            .map_err(|_| McpError::Network)?;
        let mut headers = HeaderMap::new();
        for (name, value) in self.request.headers() {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| McpError::InvalidHeaders)?;
            let mut value = HeaderValue::from_str(value).map_err(|_| McpError::InvalidHeaders)?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        if negotiated {
            headers.insert(
                "mcp-protocol-version",
                HeaderValue::from_static(crate::MCP_PROTOCOL_VERSION),
            );
        }
        if let Some(session) = session {
            let mut value = HeaderValue::from_str(session).map_err(|_| McpError::Protocol)?;
            value.set_sensitive(true);
            headers.insert("mcp-session-id", value);
        }
        let response = client
            .post(url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    McpError::Timeout
                } else {
                    McpError::Network
                }
            })?;
        if !response
            .remote_addr()
            .is_some_and(|peer| peers.contains(&peer) && self.policy.permits(peer))
        {
            return Err(McpError::Policy);
        }
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(McpError::Redirect);
        }
        if status == 404 && session.is_some() {
            return Err(McpError::SessionExpired);
        }
        if status != if notification { 202 } else { 200 } {
            return Err(McpError::HttpStatus(status));
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.limits.max_response_bytes as u64)
        {
            return Err(McpError::TooLarge);
        }
        if response.headers().contains_key("content-encoding") {
            return Err(McpError::UnsupportedContentType);
        }
        let sessions: Vec<_> = response
            .headers()
            .get_all("mcp-session-id")
            .iter()
            .collect();
        if sessions.len() > 1 {
            return Err(McpError::Protocol);
        }
        let session = sessions
            .first()
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| McpError::Protocol)?;
        if session.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 1024
                || !value.bytes().all(|c| (33..=126).contains(&c))
        }) {
            return Err(McpError::Protocol);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or("").trim());
        let event_stream = content_type == Some("text/event-stream");
        if !notification && !event_stream && content_type != Some("application/json") {
            return Err(McpError::UnsupportedContentType);
        }
        Ok(HttpReply {
            response,
            event_stream,
            session,
        })
    }

    pub async fn bounded_body(
        &self,
        reply: &mut HttpReply,
        expected_id: Option<u64>,
    ) -> Result<Option<serde_json::Value>, McpError> {
        let mut bytes = Vec::new();
        let mut stream = crate::sse::EventDecoder::default();
        let mut consumed = 0;
        while let Some(chunk) = reply.response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                McpError::Timeout
            } else {
                McpError::Network
            }
        })? {
            consumed += chunk.len();
            if consumed > self.limits.max_response_bytes {
                return Err(McpError::TooLarge);
            }
            if reply.event_stream {
                if let Some(value) = stream.push(&chunk, expected_id.ok_or(McpError::Protocol)?)? {
                    return Ok(Some(value));
                }
            } else {
                bytes.extend_from_slice(&chunk);
            }
        }
        if expected_id.is_none() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(McpError::Protocol)
            };
        }
        if reply.event_stream {
            return stream
                .finish(expected_id.unwrap())?
                .map(Some)
                .ok_or(McpError::Protocol);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| McpError::Protocol)?;
        crate::sse::rpc_result(value, expected_id.unwrap()).map(Some)
    }
}

pub(crate) async fn within<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = Result<T, McpError>>,
) -> Result<T, McpError> {
    timeout_at(deadline, future)
        .await
        .map_err(|_| McpError::Timeout)?
}

#[cfg(test)]
#[path = "../tests/support/tls.rs"]
mod tls_tests;
