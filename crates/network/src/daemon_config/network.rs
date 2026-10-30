use super::{ConfigError, StartupMode};
use crate::{RelaySelection, WireProfile};
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use serde::{Deserialize, Deserializer, de::Error};
use std::{net::SocketAddr, path::PathBuf};

/// No diagnostic representation: enabled settings may contain invitation secrets.
pub enum NetworkMode {
    Disabled,
    Enabled(Box<NetworkConfig>),
}
impl<'de> Deserialize<'de> for NetworkMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case")]
        enum On {
            Enabled(Box<NetworkConfig>),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Off(String),
            On(On),
        }
        match Input::deserialize(deserializer)
            .map_err(|_| D::Error::custom("invalid network configuration"))?
        {
            Input::Off(value) if value == "disabled" => Ok(Self::Disabled),
            Input::On(On::Enabled(config)) => Ok(Self::Enabled(config)),
            _ => Err(D::Error::custom("invalid network configuration")),
        }
    }
}

/// Caller-selected settings. Resolving referenced inputs is a separate operation.
/// ```compile_fail
/// fn diagnostics<T: std::fmt::Debug>() {}
/// diagnostics::<foundry_network::daemon_config::NetworkConfig>();
/// ```
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub profile: WireProfile,
    pub transport: TransportConfig,
    pub seeds: Vec<SeedConfig>,
    pub trust: TrustConfig,
    pub codebook: Option<CodebookConfig>,
    pub ticket_relay_compression: Option<String>,
    pub startup: StartupMode,
    pub expected_team: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    pub bind_address: SocketAddr,
    pub direct_paths: bool,
    pub relays: Vec<String>,
    pub relay_root_certificates: Vec<PathBuf>,
}
impl TransportConfig {
    pub fn relay_selection(&self) -> Result<RelaySelection, ConfigError> {
        if self.relays.len() > 16 || self.relay_root_certificates.len() > 16 {
            return Err(ConfigError::Invalid);
        }
        let mut urls = Vec::new();
        for value in &self.relays {
            let url = relay(value)?;
            if urls.contains(&url) {
                return Err(ConfigError::Invalid);
            }
            urls.push(url);
        }
        if urls.is_empty() {
            if !self.direct_paths {
                return Err(ConfigError::Invalid);
            }
            Ok(RelaySelection::Disabled)
        } else {
            Ok(RelaySelection::Custom(urls))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedConfig {
    pub endpoint: EndpointId,
    pub direct: Vec<SocketAddr>,
    pub relay: Option<String>,
}
impl SeedConfig {
    pub fn address(&self) -> Result<EndpointAddr, ConfigError> {
        let mut address = EndpointAddr::new(self.endpoint);
        for ip in &self.direct {
            address = address.with_ip_addr(*ip);
        }
        if let Some(value) = &self.relay {
            address = address.with_relay_url(relay(value)?);
        }
        Ok(address)
    }
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrustConfig {
    Unverified {},
    Proof {
        issuer: String,
        audience: String,
        token_type: String,
        jwks_file: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodebookConfig {
    pub path: PathBuf,
    pub sha256: String,
}

pub(super) fn relay(value: &str) -> Result<RelayUrl, ConfigError> {
    let url: RelayUrl = value.parse().map_err(|_| ConfigError::Invalid)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::Invalid);
    }
    Ok(url)
}
impl NetworkConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.profile.validate().map_err(|_| ConfigError::Invalid)?;
        if !self.profile.signing_prefix.ends_with('/') {
            return Err(ConfigError::Invalid);
        }
        let compression = self.compression()?;
        crate::ticket::TicketCodec::new(&self.profile, compression.as_ref())
            .map_err(|_| ConfigError::Invalid)?;
        self.transport.relay_selection()?;
        for seed in &self.seeds {
            seed.address()?;
        }
        if let TrustConfig::Proof {
            issuer,
            audience,
            token_type,
            jwks_file,
        } = &self.trust
            && ([issuer, audience, token_type]
                .iter()
                .any(|s| s.is_empty() || s.len() > 2048)
                || jwks_file.as_os_str().is_empty())
        {
            return Err(ConfigError::Invalid);
        }
        if self
            .transport
            .relay_root_certificates
            .iter()
            .any(|p| p.as_os_str().is_empty())
        {
            return Err(ConfigError::Invalid);
        }
        if let Some(book) = &self.codebook {
            book.digest()?;
            if book.path.as_os_str().is_empty() {
                return Err(ConfigError::Invalid);
            }
        }
        if self
            .expected_team
            .as_ref()
            .is_some_and(|team| team.is_empty() || team.len() > 256)
        {
            return Err(ConfigError::Invalid);
        }
        self.startup.validate(self)
    }
    pub(super) fn compression(
        &self,
    ) -> Result<Option<crate::ticket::RelayCompression>, ConfigError> {
        self.ticket_relay_compression
            .as_ref()
            .map(|suffix| {
                crate::ticket::RelayCompression::new(suffix.clone())
                    .map_err(|_| ConfigError::Invalid)
            })
            .transpose()
    }
}
impl CodebookConfig {
    pub(super) fn digest(&self) -> Result<[u8; 32], ConfigError> {
        let bytes = data_encoding::HEXLOWER
            .decode(self.sha256.to_ascii_lowercase().as_bytes())
            .map_err(|_| ConfigError::Invalid)?;
        bytes.try_into().map_err(|_| ConfigError::Invalid)
    }
}
