//! The host grammar shared by policy documents and egress destinations. Only
//! fully qualified DNS names are accepted: the spec does not define wildcards,
//! ports, IP literals or single-label names, so each is rejected rather than
//! guessed at. Matching is exact; a listed host does not admit its subdomains.
//!
//! A name that passes is still only a name: it may resolve to a loopback,
//! link-local or private address. Enforcement must check the resolved
//! addresses as well; this grammar only keeps IP literals out of the list.

use std::fmt;

const MAX_HOST_LENGTH: usize = 253;
const MAX_LABEL_LENGTH: usize = 63;

/// A validated, lowercase DNS host name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Host(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    /// Empty, or longer than 253 characters.
    Length,
    /// A character outside ASCII letters, digits, `-` and `.`. Covers schemes,
    /// ports, paths, wildcards, IPv6 literals and non-ASCII names.
    Character,
    /// An empty label (including a trailing dot), a label over 63 characters,
    /// or one that starts or ends with `-`.
    Label,
    /// One label only, or a last label that URL parsers and `inet_aton` read
    /// as a number, which makes the whole name an IPv4 literal: all digits
    /// (decimal or octal) or `0x` with hex digits, as in `127.0.0.0x1`.
    NotQualified,
}

impl Host {
    /// Parses a host, folding ASCII case since DNS names are case-insensitive.
    pub fn parse(input: &str) -> Result<Self, HostError> {
        if input.is_empty() || input.len() > MAX_HOST_LENGTH {
            return Err(HostError::Length);
        }
        if !input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        {
            return Err(HostError::Character);
        }
        let host = input.to_ascii_lowercase();
        let labels: Vec<&str> = host.split('.').collect();
        let malformed = |label: &&str| {
            label.is_empty()
                || label.len() > MAX_LABEL_LENGTH
                || label.starts_with('-')
                || label.ends_with('-')
        };
        if labels.iter().any(malformed) {
            return Err(HostError::Label);
        }
        let numeric_tail = labels.last().is_some_and(|label| is_number(label));
        if labels.len() < 2 || numeric_tail {
            return Err(HostError::NotQualified);
        }
        Ok(Self(host))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether the WHATWG host parser treats `label` as an IPv4 number. `0x`
/// alone counts: it reads as zero.
fn is_number(label: &str) -> bool {
    match label.strip_prefix("0x") {
        Some(hex) => hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        None => label.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

impl fmt::Display for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
