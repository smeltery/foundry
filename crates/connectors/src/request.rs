use crate::{Auth, Connector, ConnectorError, Endpoint, Server, validation};
use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;

/// Explicitly exposed only to the transport caller; no serialization or persistence.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Result<Self, ConnectorError> {
        if value.is_empty() || value.len() > 8192 || !validation::header_value(&value) {
            return Err(ConnectorError::InvalidCredential);
        }
        Ok(Self(value))
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[derive(Debug)]
pub enum Credentials {
    None,
    OAuthToken(Secret),
    ApiKey(Secret),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    OAuth,
    ApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationStatus {
    Configured,
    MissingCredential(AuthKind),
    UnsupportedAdapter,
    FixtureOnly,
    Rejected(ConnectorError),
}

/// Contains sensitive material. Accessors are for the transport boundary only.
/// Callers must validate resolved peer IPs and must not forward auth on redirects.
pub struct PreparedRequest {
    url: String,
    headers: BTreeMap<String, String>,
}

impl fmt::Debug for PreparedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedRequest { url: [redacted], headers: [redacted] }")
    }
}

impl PreparedRequest {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    /// No DNS is performed here. Check every actual peer, including after retries.
    pub fn validate_peer(&self, peer: IpAddr) -> Result<(), ConnectorError> {
        if validation::public_ip(peer) {
            Ok(())
        } else {
            Err(ConnectorError::UnsafeHost)
        }
    }
}

impl Connector {
    pub fn configuration_status(
        &self,
        server_id: &str,
        credentials: &Credentials,
    ) -> ConfigurationStatus {
        if self.fixture {
            return ConfigurationStatus::FixtureOnly;
        }
        match self.servers.iter().find(|server| server.id == server_id) {
            Some(server) => server.configuration_status(credentials),
            None => ConfigurationStatus::Rejected(ConnectorError::InvalidCatalog),
        }
    }

    pub fn prepare(
        &self,
        server_id: &str,
        credentials: &Credentials,
    ) -> Result<PreparedRequest, ConnectorError> {
        if self.fixture {
            return Err(ConnectorError::FixtureOnly);
        }
        self.servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or(ConnectorError::InvalidCatalog)?
            .prepare(credentials)
    }
}

impl Server {
    pub fn configuration_status(&self, credentials: &Credentials) -> ConfigurationStatus {
        match self.prepare(credentials) {
            Ok(_) => ConfigurationStatus::Configured,
            Err(ConnectorError::UnsupportedAdapter) => ConfigurationStatus::UnsupportedAdapter,
            Err(ConnectorError::MissingCredential) => {
                ConfigurationStatus::MissingCredential(match self.auth {
                    Auth::Oauth {} => AuthKind::OAuth,
                    _ => AuthKind::ApiKey,
                })
            }
            Err(error) => ConfigurationStatus::Rejected(error),
        }
    }

    pub fn prepare(&self, credentials: &Credentials) -> Result<PreparedRequest, ConnectorError> {
        self.prepare_with_policy(credentials, &crate::NetworkPolicy::default())
    }

    pub fn prepare_with_policy(
        &self,
        credentials: &Credentials,
        policy: &crate::NetworkPolicy,
    ) -> Result<PreparedRequest, ConnectorError> {
        validation::server_headers(self)?;
        let Endpoint::Remote { url } = &self.endpoint else {
            return Err(ConnectorError::UnsupportedAdapter);
        };
        policy.url(url)?;
        let mut headers = self
            .headers
            .iter()
            .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        match (&self.auth, credentials) {
            (Auth::None {}, Credentials::None) => {}
            (Auth::Oauth {}, Credentials::OAuthToken(token)) => {
                // RFC 6750 token syntax; whitespace cannot introduce extra auth fields.
                let text = token.0.trim_end_matches('=');
                if text.is_empty()
                    || !text
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-._~+/".contains(&c))
                {
                    return Err(ConnectorError::InvalidCredential);
                }
                headers.insert("authorization".into(), format!("Bearer {}", token.0));
            }
            (Auth::ApiKey { .. }, Credentials::ApiKey(key)) => {
                for value in headers.values_mut() {
                    let copies = value.matches("${credential}").count();
                    let expanded =
                        value.len() - copies * "${credential}".len() + copies * key.0.len();
                    if expanded > 16384 {
                        return Err(ConnectorError::InvalidHeaders);
                    }
                    *value = value.replace("${credential}", &key.0);
                }
            }
            (Auth::Oauth {} | Auth::ApiKey { .. }, Credentials::None) => {
                return Err(ConnectorError::MissingCredential);
            }
            _ => return Err(ConnectorError::WrongCredentialKind),
        }
        if headers
            .values()
            .any(|value| value.len() > 16384 || !validation::header_value(value))
        {
            return Err(ConnectorError::InvalidHeaders);
        }
        Ok(PreparedRequest {
            url: url.clone(),
            headers,
        })
    }
}
