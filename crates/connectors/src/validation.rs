use crate::{Auth, Catalog, ConnectorError, Endpoint, Icon, Server};
use std::collections::BTreeSet;
use std::net::IpAddr;
use url::{Host, Url};

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> Result<(), ConnectorError> {
    let mut found = BTreeSet::new();
    for id in ids {
        if !identifier(id) || !found.insert(id) {
            return Err(ConnectorError::InvalidCatalog);
        }
    }
    Ok(())
}

pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only global unicast; exclude documentation and special-use ranges.
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

pub(crate) fn url(value: &str, endpoint: bool) -> Result<Url, ConnectorError> {
    if value.contains("${") {
        return Err(ConnectorError::UnsupportedQueryAuth);
    }
    if !(value.starts_with("https://") || value.starts_with("http://"))
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(ConnectorError::InvalidUrl);
    }
    let parsed = Url::parse(value).map_err(|_| ConnectorError::InvalidUrl)?;
    let authority = value
        .split_once("://")
        .ok_or(ConnectorError::InvalidUrl)?
        .1
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.contains('@')
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port() == Some(0)
        || (endpoint && parsed.fragment().is_some())
    {
        return Err(ConnectorError::InvalidUrl);
    }
    let safe = match parsed.host().ok_or(ConnectorError::InvalidUrl)? {
        Host::Ipv4(ip) => public_ip(ip.into()),
        Host::Ipv6(ip) => public_ip(ip.into()),
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.');
            domain.contains('.')
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                })
                && ![
                    "localhost",
                    "local",
                    "localdomain",
                    "internal",
                    "home.arpa",
                    "lan",
                ]
                .iter()
                .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
        }
    };
    if !safe {
        return Err(ConnectorError::UnsafeHost);
    }
    Ok(parsed)
}

pub(crate) fn header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

pub(crate) fn header_value(value: &str) -> bool {
    value.bytes().all(|c| (32..=126).contains(&c))
}

pub(crate) fn server(server: &Server) -> Result<(), ConnectorError> {
    if !identifier(&server.id) {
        return Err(ConnectorError::InvalidCatalog);
    }
    match &server.endpoint {
        Endpoint::Remote { url: endpoint } => {
            url(endpoint, true)?;
        }
        Endpoint::Adapter { id, .. } if !identifier(id) => {
            return Err(ConnectorError::InvalidCatalog);
        }
        Endpoint::Adapter { .. } => {}
    }
    server_headers(server)
}

pub(crate) fn server_headers(server: &Server) -> Result<(), ConnectorError> {
    if !identifier(&server.id) {
        return Err(ConnectorError::InvalidCatalog);
    }
    let mut names = BTreeSet::new();
    let mut substitutions = 0;
    if server.headers.len() > 128 {
        return Err(ConnectorError::InvalidHeaders);
    }
    for (name, value) in &server.headers {
        let lower = name.to_ascii_lowercase();
        if name.len() > 256
            || value.len() > 16384
            || !header_name(name)
            || !header_value(value)
            || !names.insert(lower.clone())
            || [
                "host",
                "connection",
                "content-length",
                "transfer-encoding",
                "proxy-authorization",
                "proxy-connection",
                "upgrade",
                "te",
                "trailer",
            ]
            .contains(&lower.as_str())
        {
            return Err(ConnectorError::InvalidHeaders);
        }
        if lower == "authorization" && !matches!(server.auth, Auth::ApiKey { .. }) {
            return Err(ConnectorError::InvalidHeaders);
        }
        substitutions += value.matches("${credential}").count();
        if value.replace("${credential}", "").contains("${") {
            return Err(ConnectorError::InvalidTemplate);
        }
    }
    match server.auth {
        Auth::ApiKey { .. } if substitutions == 0 => Err(ConnectorError::InvalidTemplate),
        Auth::None {} | Auth::Oauth {} if substitutions != 0 => {
            Err(ConnectorError::InvalidTemplate)
        }
        _ => Ok(()),
    }
}

pub(crate) fn catalog(catalog: &Catalog) -> Result<(), ConnectorError> {
    if catalog.schema_version != 1 || catalog.connectors.is_empty() {
        return Err(ConnectorError::InvalidCatalog);
    }
    unique(catalog.connectors.iter().map(|c| c.id.as_str()))?;
    for connector in &catalog.connectors {
        if connector.display_name.is_empty()
            || connector.description.is_empty()
            || connector.servers.is_empty()
        {
            return Err(ConnectorError::InvalidCatalog);
        }
        for value in [&connector.homepage, &connector.documentation]
            .into_iter()
            .flatten()
        {
            url(value, false)?;
        }
        match &connector.icon {
            Some(Icon::Remote { url: value }) => {
                url(value, false)?;
            }
            Some(Icon::Local { path })
                if path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                    || path.chars().any(|c| c.is_control() || "\\:%".contains(c)) =>
            {
                return Err(ConnectorError::InvalidCatalog);
            }
            Some(Icon::Local { .. }) | None => {}
        }
        unique(connector.servers.iter().map(|s| s.id.as_str()))?;
        for entry in &connector.servers {
            server(entry)?;
        }
        if let Some(skills) = &connector.skills {
            if skills.is_empty() {
                return Err(ConnectorError::InvalidCatalog);
            }
            unique(skills.iter().map(|s| s.id.as_str()))?;
            for skill in skills {
                url(&skill.url, false)?;
                if ![40, 64].contains(&skill.revision.len())
                    || !skill
                        .revision
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                {
                    return Err(ConnectorError::InvalidCatalog);
                }
            }
        }
    }
    Ok(())
}
