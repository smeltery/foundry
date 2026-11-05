use super::network::admit_addresses;
use super::{
    Admission, Completion, CompletionRequest, EndpointError, EndpointKey, EndpointSettings,
    FailureCategory, InvalidField, OptIn, Resolve, format,
};
use crate::catalog::{EndpointFormat, ModelId, ProviderDescriptor, ProviderId, ProviderKind};
use reqwest::header::{HeaderMap, HeaderValue};
use rustls_platform_verifier::BuilderVerifierExt;
use std::error::Error as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::{fmt, time::Duration};
use url::{Host, Url};

/// Largest reply body read from an endpoint.
const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;

/// A validated client for one custom model endpoint. Every call resolves the
/// host once, admits the answers by network class and connects only to them.
/// Proxy variables are ignored and redirects are never followed.
pub struct LocalEndpoint {
    provider_id: ProviderId,
    format: EndpointFormat,
    url: Url,
    models: Vec<ModelId>,
    opt_in: Vec<OptIn>,
    timeout: Duration,
    key: Option<EndpointKey>,
    tls: Option<rustls::ClientConfig>,
}

impl fmt::Debug for LocalEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalEndpoint")
            .field("provider_id", &self.provider_id)
            .field("format", &self.format)
            .field("url", &"[redacted]")
            .field("models", &self.models)
            .field("opt_in", &self.opt_in)
            .finish_non_exhaustive()
    }
}

impl LocalEndpoint {
    /// Takes the provider ID and wire format from a `local_endpoint` descriptor,
    /// using the catalog default format when it names none.
    pub fn new(
        descriptor: &ProviderDescriptor,
        settings: EndpointSettings,
    ) -> Result<Self, EndpointError> {
        if descriptor.kind != ProviderKind::LocalEndpoint || descriptor.validate().is_err() {
            return Err(EndpointError::InvalidRequest(InvalidField::Descriptor));
        }
        let url = settings.validate()?;
        let endpoint = Self {
            provider_id: descriptor.provider_id.clone(),
            format: descriptor
                .endpoint_format
                .unwrap_or(ProviderDescriptor::LOCAL_ENDPOINT_FORMAT),
            url,
            models: settings.models,
            opt_in: settings.opt_in,
            timeout: settings.timeout,
            key: settings.key,
            tls: None,
        };
        // A literal address is refused now rather than at the first call.
        if let Some(ip) = endpoint.literal_ip() {
            admit_addresses(vec![ip], endpoint.is_https(), &endpoint.opt_in)?;
        }
        Ok(endpoint)
    }

    /// Replaces the platform certificate verifier, for a private CA.
    pub fn with_tls_config(mut self, tls: rustls::ClientConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn format(&self) -> EndpointFormat {
        self.format
    }

    pub fn models(&self) -> &[ModelId] {
        &self.models
    }

    /// Resolves the host and admits it by network class, within the timeout.
    pub async fn admit(&self, resolver: &impl Resolve) -> Result<Admission, EndpointError> {
        tokio::time::timeout(self.timeout, self.resolve(resolver))
            .await
            .map_err(|_| self.failure(FailureCategory::Timeout))?
    }

    /// Sends one completion. The timeout bounds the whole call.
    pub async fn complete(
        &self,
        resolver: &impl Resolve,
        request: &CompletionRequest,
    ) -> Result<Completion, EndpointError> {
        if !self.models.contains(&request.model) {
            return Err(EndpointError::UnknownModel);
        }
        if !request.has_valid_messages() {
            return Err(EndpointError::InvalidRequest(InvalidField::Messages));
        }
        if request.max_output_tokens == 0 {
            return Err(EndpointError::InvalidRequest(InvalidField::MaxOutputTokens));
        }
        let body = format::request_body(self.format, request);
        tokio::time::timeout(self.timeout, self.exchange(resolver, &body))
            .await
            .map_err(|_| self.failure(FailureCategory::Timeout))?
    }

    async fn resolve(&self, resolver: &impl Resolve) -> Result<Admission, EndpointError> {
        let addresses = match (self.literal_ip(), self.url.host()) {
            (Some(ip), _) => vec![ip],
            (None, Some(Host::Domain(host))) => match resolver.lookup(host).await {
                Ok(addresses) if !addresses.is_empty() => addresses,
                _ => return Err(self.failure(FailureCategory::Dns)),
            },
            _ => return Err(EndpointError::InvalidRequest(InvalidField::BaseUrl)),
        };
        let (network_class, addresses) = admit_addresses(addresses, self.is_https(), &self.opt_in)?;
        Ok(Admission {
            network_class,
            format: self.format,
            addresses,
        })
    }

    async fn exchange(
        &self,
        resolver: &impl Resolve,
        body: &serde_json::Value,
    ) -> Result<Completion, EndpointError> {
        let admission = self.resolve(resolver).await?;
        let port = self
            .url
            .port_or_known_default()
            .ok_or(EndpointError::InvalidRequest(InvalidField::BaseUrl))?;
        let peers: Vec<SocketAddr> = admission
            .addresses
            .iter()
            .map(|ip| SocketAddr::new(*ip, port))
            .collect();
        let client = self.client(&peers)?;
        let body = serde_json::to_vec(body).map_err(|_| self.failure(FailureCategory::Protocol))?;
        let mut response = client
            .post(self.call_url()?)
            .headers(self.headers()?)
            .body(body)
            .send()
            .await
            .map_err(|error| self.failure(transport_category(&error)))?;
        // The pinned resolution makes another peer unexpected; refuse it anyway.
        if !response
            .remote_addr()
            .is_some_and(|peer| peers.contains(&peer))
        {
            return Err(EndpointError::NotPermitted);
        }
        let status = response.status().as_u16();
        if status != 200 {
            return Err(self.failure(FailureCategory::Http(status)));
        }
        let is_json = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(';').next().unwrap_or("").trim() == "application/json"
            });
        if !is_json || response.headers().contains_key("content-encoding") {
            return Err(self.failure(FailureCategory::Protocol));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.failure(transport_category(&error)))?
        {
            if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
                return Err(self.failure(FailureCategory::Protocol));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .ok()
            .and_then(|reply| format::parse_reply(self.format, &reply))
            .ok_or_else(|| self.failure(FailureCategory::Protocol))
    }

    /// A fresh client pinned to the vetted peers, so no second lookup happens.
    fn client(&self, peers: &[SocketAddr]) -> Result<reqwest::Client, EndpointError> {
        let tls = match &self.tls {
            Some(tls) => tls.clone(),
            None => rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|_| self.failure(FailureCategory::Tls))?
            .with_platform_verifier()
            .map_err(|_| self.failure(FailureCategory::Tls))?
            .with_no_client_auth(),
        };
        let host = self
            .url
            .host_str()
            .ok_or(EndpointError::InvalidRequest(InvalidField::BaseUrl))?;
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .http1_only()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .tls_sslkeylogfile(false)
            .tls_backend_preconfigured(tls)
            .timeout(self.timeout)
            .connect_timeout(self.timeout)
            .resolve_to_addrs(host, peers)
            .build()
            .map_err(|_| self.failure(FailureCategory::Connect))
    }

    fn call_url(&self) -> Result<Url, EndpointError> {
        let base = self.url.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}/{}", format::path(self.format)))
            .map_err(|_| EndpointError::InvalidRequest(InvalidField::BaseUrl))
    }

    fn headers(&self) -> Result<HeaderMap, EndpointError> {
        let mut headers = HeaderMap::new();
        headers.insert("accept", HeaderValue::from_static("application/json"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        if let Some(key) = &self.key {
            let value = match self.format {
                EndpointFormat::Message => key.expose().to_owned(),
                _ => format!("Bearer {}", key.expose()),
            };
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| EndpointError::InvalidRequest(InvalidField::Key))?;
            value.set_sensitive(true);
            headers.insert(format::key_header(self.format), value);
        }
        Ok(headers)
    }

    fn literal_ip(&self) -> Option<IpAddr> {
        match self.url.host()? {
            Host::Ipv4(ip) => Some(ip.into()),
            Host::Ipv6(ip) => Some(ip.into()),
            Host::Domain(_) => None,
        }
    }

    fn is_https(&self) -> bool {
        self.url.scheme() == "https"
    }

    fn failure(&self, category: FailureCategory) -> EndpointError {
        EndpointError::Failure {
            provider_id: self.provider_id.clone(),
            category,
        }
    }
}

/// Maps a transport error to a category without keeping its text, which can
/// contain the URL.
fn transport_category(error: &reqwest::Error) -> FailureCategory {
    if error.is_timeout() {
        return FailureCategory::Timeout;
    }
    if error.source().is_some_and(is_tls) {
        FailureCategory::Tls
    } else {
        FailureCategory::Connect
    }
}

/// Whether a TLS error is anywhere in the chain. A wrapped I/O error hides its
/// inner error from `source`, so it is unwrapped explicitly.
fn is_tls(cause: &(dyn std::error::Error + 'static)) -> bool {
    if cause.is::<rustls::Error>() {
        return true;
    }
    if let Some(inner) = cause
        .downcast_ref::<std::io::Error>()
        .and_then(|io| io.get_ref())
    {
        return is_tls(inner);
    }
    cause.source().is_some_and(is_tls)
}
