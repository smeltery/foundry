use crate::{Channel, NetworkError, Result, WireProfile};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey,
    endpoint::{Connection, presets},
};
use std::{net::SocketAddr, time::Duration};

#[derive(Debug, Clone)]
pub enum RelaySelection {
    Disabled,
    Custom(Vec<RelayUrl>),
}

pub struct EndpointConfig {
    pub profile: WireProfile,
    pub secret_key: SecretKey,
    pub relay: RelaySelection,
    pub bind_address: SocketAddr,
    pub direct_paths: bool,
    /// DER roots for explicit private relays. Empty uses standard public roots.
    /// There is deliberately no TLS verification bypass option.
    pub relay_root_certificates: Vec<Vec<u8>>,
}

pub struct NetworkEndpoint {
    endpoint: Endpoint,
    profile: WireProfile,
}
impl NetworkEndpoint {
    pub async fn bind(config: EndpointConfig) -> Result<Self> {
        config.profile.validate()?;
        let relay = match config.relay {
            RelaySelection::Disabled if config.direct_paths => RelayMode::Disabled,
            RelaySelection::Disabled => return Err(NetworkError::Invalid("no enabled transport")),
            RelaySelection::Custom(urls) => {
                if urls.is_empty() || urls.len() > 16 {
                    return Err(NetworkError::Invalid(
                        "one to sixteen explicit relays required",
                    ));
                }
                for (index, url) in urls.iter().enumerate() {
                    if url.scheme() != "https"
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some()
                        || urls[..index].contains(url)
                    {
                        return Err(NetworkError::Invalid(
                            "relays must be distinct HTTPS endpoints without credentials",
                        ));
                    }
                }
                RelayMode::custom(urls)
            }
        };
        if config.relay_root_certificates.len() > 16
            || config
                .relay_root_certificates
                .iter()
                .any(|cert| cert.is_empty() || cert.len() > 64 * 1024)
        {
            return Err(NetworkError::Invalid("relay certificates exceed bounds"));
        }
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(config.secret_key)
            .alpns(vec![
                config.profile.team_alpn.clone(),
                config.profile.join_alpn.clone(),
            ])
            .relay_mode(relay)
            .clear_ip_transports();
        if config.direct_paths {
            builder = builder.bind_addr(config.bind_address).map_err(transport)?;
        }
        if !config.relay_root_certificates.is_empty() {
            builder = builder.ca_tls_config(iroh::tls::CaTlsConfig::custom_roots(
                config.relay_root_certificates.into_iter().map(Into::into),
            ));
        }
        let endpoint = builder.bind().await.map_err(transport)?;
        Ok(Self {
            endpoint,
            profile: config.profile,
        })
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }
    pub fn address(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Establishes cryptographic peer identity only, not membership authorization.
    pub async fn connect(
        &self,
        peer: EndpointAddr,
        channel: Channel,
        timeout: Duration,
    ) -> Result<Connection> {
        positive(timeout)?;
        tokio::time::timeout(
            timeout,
            self.endpoint.connect(peer, self.profile.alpn(channel)),
        )
        .await
        .map_err(|_| NetworkError::TimedOut)?
        .map_err(transport)
    }

    /// The caller must authorize the peer before processing application messages.
    pub async fn accept(&self, timeout: Duration) -> Result<Connection> {
        positive(timeout)?;
        tokio::time::timeout(timeout, async {
            self.endpoint
                .accept()
                .await
                .ok_or(NetworkError::Closed)?
                .await
                .map_err(transport)
        })
        .await
        .map_err(|_| NetworkError::TimedOut)?
    }

    pub async fn online(&self, timeout: Duration) -> Result<()> {
        positive(timeout)?;
        tokio::time::timeout(timeout, self.endpoint.online())
            .await
            .map_err(|_| NetworkError::TimedOut)
    }

    /// Await closure of all connections and endpoint-owned background tasks.
    pub async fn shutdown(self) {
        self.endpoint.close().await;
    }
}
fn positive(timeout: Duration) -> Result<()> {
    if timeout.is_zero() {
        Err(NetworkError::Invalid("positive operation timeout required"))
    } else {
        Ok(())
    }
}
fn transport(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::Transport(error.to_string())
}
