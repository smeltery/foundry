//! Loopback URIs for redirects and the issuer. Only `http` to the IP literals
//! `127.0.0.1` and `[::1]` is accepted, with an optional port. The name
//! `localhost` is refused because it can resolve to a non-loopback address.
//! Matching is byte-exact: no case folding, default-port or path
//! normalization, and no port wildcard.
use std::fmt;

const SCHEME: &str = "http://";
const HOSTS: [&str; 2] = ["127.0.0.1", "[::1]"];
const MAX_LENGTH: usize = 2048;

/// A validated loopback URI.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoopbackUri(String);

impl LoopbackUri {
    /// Parses `input`, refusing anything but a loopback `http` URI without
    /// user information or fragment.
    pub fn parse(input: &str) -> Option<Self> {
        let printable = input.bytes().all(|byte| byte.is_ascii_graphic());
        if input.len() > MAX_LENGTH || !printable || input.contains('#') {
            return None;
        }
        let rest = input.strip_prefix(SCHEME)?;
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, _) = rest.split_at(authority_end);
        let (host, port) = match authority.rsplit_once(':') {
            // `[::1]` holds colons of its own, so only a colon after the
            // closing bracket starts a port.
            Some((host, port)) if !port.contains(']') => (host, Some(port)),
            _ => (authority, None),
        };
        let valid_port = port.is_none_or(|port| {
            !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|number| number != 0)
        });
        (HOSTS.contains(&host) && valid_port).then(|| Self(input.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LoopbackUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
