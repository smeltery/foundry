use crate::{ConnectorError, validation};
use std::net::{IpAddr, SocketAddr};
use url::{Host, Url};

/// Local access is scoped to one literal loopback HTTP origin, never all private hosts.
#[derive(Clone, Default)]
pub struct NetworkPolicy {
    local: Option<Url>,
}

impl std::fmt::Debug for NetworkPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NetworkPolicy { origin: [redacted] }")
    }
}

impl NetworkPolicy {
    pub fn local_http(origin: &str) -> Result<Self, ConnectorError> {
        let url = Url::parse(origin).map_err(|_| ConnectorError::InvalidUrl)?;
        if url.scheme() != "http"
            || !matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
                && !matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_loopback())
            || url.port_or_known_default().is_none_or(|port| port == 0)
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || origin.contains('@')
            || origin
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err(ConnectorError::UnsafeHost);
        }
        Ok(Self { local: Some(url) })
    }

    #[cfg(test)]
    pub(crate) fn local_https(origin: &str) -> Self {
        let url = Url::parse(origin).unwrap();
        assert_eq!(url.scheme(), "https");
        let mut plain = url.clone();
        plain.set_scheme("http").unwrap();
        Self::local_http(plain.as_str()).unwrap();
        Self { local: Some(url) }
    }

    pub(crate) fn url(&self, endpoint: &str) -> Result<Url, ConnectorError> {
        if let Some(local) = &self.local {
            let url = Url::parse(endpoint).map_err(|_| ConnectorError::InvalidUrl)?;
            if url.origin() != local.origin()
                || url.fragment().is_some()
                || endpoint.contains('@')
                || endpoint.contains("${")
                || !url.username().is_empty()
                || url.password().is_some()
                || endpoint
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
            {
                return Err(ConnectorError::UnsafeHost);
            }
            return Ok(url);
        }
        let url = validation::url(endpoint, true)?;
        if url.scheme() != "https" {
            return Err(ConnectorError::InsecureTransport);
        }
        Ok(url)
    }

    pub(crate) fn permits(&self, peer: SocketAddr) -> bool {
        match &self.local {
            Some(url) => {
                url.host_str()
                    .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                    == Some(peer.ip())
                    && url.port_or_known_default() == Some(peer.port())
            }
            None => validation::public_ip(peer.ip()),
        }
    }

    pub(crate) fn is_local(&self) -> bool {
        self.local.is_some()
    }

    pub(crate) fn checked_peers(
        &self,
        mut peers: Vec<SocketAddr>,
    ) -> Result<Vec<SocketAddr>, crate::McpError> {
        if peers.is_empty()
            || peers.len() > 64
            || peers
                .iter()
                .any(|peer| peer.port() == 0 || !self.permits(*peer))
        {
            return Err(crate::McpError::Policy);
        }
        peers.sort();
        peers.dedup();
        Ok(peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_dns_answers_fail_before_any_transport_is_built() {
        let policy = NetworkPolicy::default();
        let public = "8.8.8.8:443".parse().unwrap();
        let private = "127.0.0.1:443".parse().unwrap();
        assert!(policy.checked_peers(vec![public]).is_ok());
        assert!(policy.checked_peers(vec![public, private]).is_err());
        assert!(policy.checked_peers(vec![]).is_err());
        assert!(policy.checked_peers(vec![public; 65]).is_err());
    }
}
