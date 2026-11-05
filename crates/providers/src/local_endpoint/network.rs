use super::EndpointError;
use crate::catalog::EndpointFormat;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Most DNS answers a single endpoint admission accepts.
const MAX_ADDRESSES: usize = 64;

/// The address class an endpoint resolves to. Names are provisional wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NetworkClass {
    Loopback,
    Lan,
    Tailnet,
    /// Always refused, including cloud metadata addresses.
    LinkLocal,
    /// Unspecified, multicast, broadcast, reserved and IPv4-embedding IPv6
    /// addresses. Always refused: they can reach local hosts by other routes.
    Reserved,
    Public,
}

/// A private address class an endpoint explicitly opts into. Public endpoints
/// need no opt-in; link-local and reserved addresses cannot be opted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptIn {
    Loopback,
    Lan,
    Tailnet,
}

impl OptIn {
    fn class(self) -> NetworkClass {
        match self {
            Self::Loopback => NetworkClass::Loopback,
            Self::Lan => NetworkClass::Lan,
            Self::Tailnet => NetworkClass::Tailnet,
        }
    }
}

/// Cloud metadata addresses that sit outside 169.254/16 and fe80::/10, inside
/// the tailnet and LAN ranges. They are classed as link-local, so no opt-in
/// admits them.
const METADATA_V4: [Ipv4Addr; 1] = [Ipv4Addr::new(100, 100, 100, 200)];
const METADATA_V6: [Ipv6Addr; 1] = [Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)];

/// Classifies one address. An IPv4-mapped IPv6 address takes its IPv4 class.
pub fn classify(ip: IpAddr) -> NetworkClass {
    let is_metadata = match ip {
        IpAddr::V4(ip) => METADATA_V4.contains(&ip),
        IpAddr::V6(ip) => {
            METADATA_V6.contains(&ip)
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| METADATA_V4.contains(&v4))
        }
    };
    if is_metadata {
        return NetworkClass::LinkLocal;
    }
    match ip {
        IpAddr::V4(ip) => classify_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => classify_v4(mapped),
            None => classify_v6(ip),
        },
    }
}

fn classify_v4(ip: Ipv4Addr) -> NetworkClass {
    let [a, b, ..] = ip.octets();
    if ip.is_loopback() {
        NetworkClass::Loopback
    } else if ip.is_private() {
        NetworkClass::Lan
    } else if a == 100 && (64..=127).contains(&b) {
        NetworkClass::Tailnet
    } else if ip.is_link_local() {
        NetworkClass::LinkLocal
    } else if a == 0 || a >= 224 {
        NetworkClass::Reserved
    } else {
        NetworkClass::Public
    }
}

fn classify_v6(ip: Ipv6Addr) -> NetworkClass {
    let segments = ip.segments();
    if ip.is_loopback() {
        NetworkClass::Loopback
    } else if segments[0] & 0xfe00 == 0xfc00 {
        NetworkClass::Lan
    } else if segments[0] & 0xffc0 == 0xfe80 {
        NetworkClass::LinkLocal
    } else if ip.is_unspecified()
        || ip.is_multicast()
        // IPv4-compatible, 6to4 and NAT64 addresses embed an IPv4 destination.
        || segments[..6] == [0; 6]
        || segments[0] == 0x2002
        || segments[..2] == [0x64, 0xff9b]
    {
        NetworkClass::Reserved
    } else {
        NetworkClass::Public
    }
}

/// Resolves an endpoint host name. Tests inject a fixture resolver so no real
/// name service is used.
pub trait Resolve: Send + Sync {
    fn lookup(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send;
}

/// The operating system resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    async fn lookup(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let answers = tokio::net::lookup_host((host, 0)).await?;
        Ok(answers
            .map(|address| address.ip())
            .take(MAX_ADDRESSES + 1)
            .collect())
    }
}

/// An endpoint admitted for one call. The vetted addresses are the only ones
/// the call may connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub network_class: NetworkClass,
    pub format: EndpointFormat,
    pub(super) addresses: Vec<IpAddr>,
}

/// Decides the class from every answer before any connection is made.
pub(super) fn admit_addresses(
    mut addresses: Vec<IpAddr>,
    https: bool,
    opt_in: &[OptIn],
) -> Result<(NetworkClass, Vec<IpAddr>), EndpointError> {
    if addresses.len() > MAX_ADDRESSES {
        return Err(EndpointError::NotPermitted);
    }
    addresses.sort();
    addresses.dedup();
    let first = *addresses.first().ok_or(EndpointError::NotPermitted)?;
    let class = classify(first);
    if addresses.iter().any(|ip| classify(*ip) != class) {
        return Err(EndpointError::NotPermitted);
    }
    match class {
        NetworkClass::LinkLocal | NetworkClass::Reserved => Err(EndpointError::NotPermitted),
        NetworkClass::Public if !https => Err(EndpointError::InsecureTransport),
        NetworkClass::Public => Ok((class, addresses)),
        _ if opt_in.iter().any(|choice| choice.class() == class) => Ok((class, addresses)),
        _ => Err(EndpointError::NotPermitted),
    }
}
