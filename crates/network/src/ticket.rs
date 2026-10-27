//! Invitation bytes with deployment-supplied prefixes and relay compression.
//! Tickets contain bearer secrets: serialize only to an intentional invitation sink.
//! Decoding does not authenticate a team, authorize joining or contact any peer.
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::ticket::Ticket>();
//! ```
use crate::{NetworkError, Result, WireProfile};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use iroh::{EndpointAddr, EndpointId};
use std::net::{IpAddr, SocketAddr};
#[path = "ticket_text.rs"]
mod text;
pub use text::display_name;

pub const MAX_TICKET_BYTES: usize = 65_536;

#[derive(Clone, PartialEq, Eq)]
pub struct Ticket {
    pub team: String,
    pub secret: String,
    pub name: String,
    pub relay: Option<String>,
    pub hint_relay: Option<String>,
    pub hint_ips: Vec<SocketAddr>,
    pub peers: Vec<EndpointId>,
}

/// Canonical hostname suffix, including leading and trailing dots. No default.
pub struct RelayCompression(String);
impl RelayCompression {
    pub fn new(suffix: String) -> Result<Self> {
        if suffix.len() < 3
            || suffix.len() > 200
            || !suffix.starts_with('.')
            || !suffix.ends_with('.')
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
        {
            return Err(invalid());
        }
        Ok(Self(suffix))
    }
    fn region<'a>(&self, relay: &'a str) -> Option<&'a str> {
        let host = relay.strip_prefix("https://")?.trim_end_matches('/');
        let region = host
            .strip_suffix(&self.0)
            .or_else(|| host.strip_suffix(self.0.trim_end_matches('.')))?;
        (!region.is_empty()
            && region.len() < 32
            && region
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'))
        .then_some(region)
    }
}

pub struct TicketCodec<'a> {
    profile: &'a WireProfile,
    compression: Option<&'a RelayCompression>,
}
impl<'a> TicketCodec<'a> {
    pub fn new(
        profile: &'a WireProfile,
        compression: Option<&'a RelayCompression>,
    ) -> Result<Self> {
        profile.validate()?;
        for (index, prefix) in profile.ticket_prefixes.iter().enumerate() {
            if profile.ticket_prefixes[..index]
                .iter()
                .any(|other| prefix.starts_with(other) || other.starts_with(prefix))
            {
                return Err(NetworkError::Invalid("ticket prefixes overlap"));
            }
        }
        Ok(Self {
            profile,
            compression,
        })
    }

    pub fn encode(&self, ticket: &Ticket) -> Result<String> {
        // Bound peer bytes before constructing the payload. Text and IP fields
        // have independent wire truncation rules below.
        if ticket.peers.len() > MAX_TICKET_BYTES / 32 {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        for value in [&ticket.team, &ticket.secret] {
            if value.len() != 32 {
                return Err(invalid());
            }
            bytes.extend(HEXLOWER.decode(value.as_bytes()).map_err(|_| invalid())?);
        }
        put_text(
            &mut bytes,
            &ticket.name.chars().take(60).collect::<String>(),
        );
        for relay in [&ticket.relay, &ticket.hint_relay] {
            let relay = relay.as_deref().unwrap_or("");
            if let Some(region) = self.compression.and_then(|rule| rule.region(relay)) {
                bytes.push(255);
                put_text(&mut bytes, region);
            } else {
                put_text(&mut bytes, relay);
            }
        }
        bytes.push(ticket.hint_ips.len().min(2) as u8);
        for address in ticket.hint_ips.iter().take(2) {
            match address.ip() {
                IpAddr::V4(ip) => {
                    bytes.push(4);
                    bytes.extend(ip.octets());
                }
                IpAddr::V6(ip) => {
                    bytes.push(6);
                    bytes.extend(ip.octets());
                }
            }
            bytes.extend(address.port().to_be_bytes());
        }
        for peer in &ticket.peers {
            bytes.extend(peer.as_bytes());
        }
        if bytes.len() > MAX_TICKET_BYTES {
            return Err(invalid());
        }
        Ok(format!(
            "{}{}",
            self.profile.ticket_prefixes[0],
            BASE64URL_NOPAD.encode(&bytes)
        ))
    }

    pub fn decode(&self, text: &str) -> Result<Ticket> {
        let text = text.trim().rsplit('#').next().ok_or_else(invalid)?;
        let encoded = self
            .profile
            .ticket_prefixes
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
            .ok_or_else(invalid)?;
        if encoded.len() > MAX_TICKET_BYTES.div_ceil(3) * 4 {
            return Err(invalid());
        }
        let bytes = BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .map_err(|_| invalid())?;
        if bytes.len() > MAX_TICKET_BYTES {
            return Err(invalid());
        }
        let mut cursor = Cursor {
            remaining: &bytes,
            compression: self.compression,
        };
        let team = HEXLOWER.encode(cursor.take(16)?);
        let secret = HEXLOWER.encode(cursor.take(16)?);
        let name = cursor.text()?;
        let relay = nonempty(cursor.text()?);
        let hint_relay = nonempty(cursor.text()?);
        let count = cursor.take(1)?[0];
        let mut hint_ips = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let family = cursor.take(1)?[0];
            let ip = match family {
                4 => IpAddr::from(<[u8; 4]>::try_from(cursor.take(4)?).map_err(|_| invalid())?),
                6 => IpAddr::from(<[u8; 16]>::try_from(cursor.take(16)?).map_err(|_| invalid())?),
                _ => return Err(invalid()),
            };
            let port = u16::from_be_bytes(cursor.take(2)?.try_into().map_err(|_| invalid())?);
            hint_ips.push(SocketAddr::new(ip, port));
        }
        if cursor.remaining.is_empty() || !cursor.remaining.len().is_multiple_of(32) {
            return Err(invalid());
        }
        let mut peers = Vec::with_capacity(cursor.remaining.len() / 32);
        while !cursor.remaining.is_empty() {
            peers.push(
                EndpointId::from_bytes(cursor.take(32)?.try_into().map_err(|_| invalid())?)
                    .map_err(|_| invalid())?,
            );
        }
        Ok(Ticket {
            team,
            secret,
            name,
            relay,
            hint_relay,
            hint_ips,
            peers,
        })
    }
}

impl Ticket {
    /// Team relay applies to every peer; otherwise only the first gets relay/IP hints.
    /// An invalid relay URL is ignored here. No network request is performed.
    pub fn peer_addresses(&self) -> Vec<EndpointAddr> {
        self.peers
            .iter()
            .enumerate()
            .map(|(index, peer)| {
                let mut address = EndpointAddr::new(*peer);
                let relay = self
                    .relay
                    .as_deref()
                    .or_else(|| (index == 0).then_some(self.hint_relay.as_deref()).flatten());
                if let Some(relay) = relay.and_then(|value| value.parse().ok()) {
                    address = address.with_relay_url(relay);
                }
                if index == 0 {
                    for ip in &self.hint_ips {
                        address = address.with_ip_addr(*ip);
                    }
                }
                address
            })
            .collect()
    }
}

struct Cursor<'a> {
    remaining: &'a [u8],
    compression: Option<&'a RelayCompression>,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let (value, rest) = self.remaining.split_at_checked(count).ok_or_else(invalid)?;
        self.remaining = rest;
        Ok(value)
    }
    fn text(&mut self) -> Result<String> {
        let length = self.take(1)?[0];
        if length == 255 {
            let suffix = &self
                .compression
                .ok_or(NetworkError::Invalid("ticket compression is unconfigured"))?
                .0;
            let count = usize::from(self.take(1)?[0]);
            return Ok(format!(
                "https://{}{suffix}/",
                String::from_utf8_lossy(self.take(count)?)
            ));
        }
        Ok(String::from_utf8_lossy(self.take(usize::from(length))?).into_owned())
    }
}
fn put_text(bytes: &mut Vec<u8>, value: &str) {
    let mut length = value.len().min(250);
    while !value.is_char_boundary(length) {
        length -= 1;
    }
    bytes.push(length as u8);
    bytes.extend_from_slice(&value.as_bytes()[..length]);
}
fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}
fn invalid() -> NetworkError {
    NetworkError::Invalid("invalid invitation ticket")
}
