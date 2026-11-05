//! Loopback URIs for redirects and the issuer. Only `http` to the IP literals
//! `127.0.0.1` and `[::1]` is accepted, with an optional port. The name
//! `localhost` is refused because it can resolve to a non-loopback address.
//!
//! A redirect matches a registered one as loopback redirects require: the
//! host and everything after the authority (path and query) must be equal
//! byte for byte, while the port may differ, since a native client picks a
//! free port when it starts listening. There is no case folding or path
//! normalization.
use std::fmt;

const SCHEME: &str = "http://";
const HOSTS: [&str; 2] = ["127.0.0.1", "[::1]"];
const MAX_LENGTH: usize = 2048;

/// A validated loopback URI.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoopbackUri {
    text: String,
    /// Byte offset of the end of the host within `text`.
    host_end: usize,
    /// Byte offset of the path or query, after any port.
    rest_start: usize,
}

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
        let authority = &rest[..authority_end];
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
        (HOSTS.contains(&host) && valid_port).then(|| Self {
            text: input.to_owned(),
            host_end: SCHEME.len() + host.len(),
            rest_start: SCHEME.len() + authority_end,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether `presented` may stand for this registered redirect: the same
    /// host, path and query on any port.
    pub fn admits(&self, presented: &LoopbackUri) -> bool {
        self.text[..self.host_end] == presented.text[..presented.host_end]
            && self.text[self.rest_start..] == presented.text[presented.rest_start..]
    }
}

impl fmt::Display for LoopbackUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}
