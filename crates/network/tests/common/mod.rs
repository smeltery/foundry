use foundry_network::{EndpointConfig, RelaySelection, WireProfile};
use iroh::SecretKey;
use std::net::{Ipv4Addr, SocketAddr};
pub fn profile() -> WireProfile {
    WireProfile {
        team_alpn: b"fixture/team/1".to_vec(),
        join_alpn: b"fixture/join/1".to_vec(),
        protocol: 1,
        signing_prefix: "fixture/signature/1/".into(),
        invitation_endpoint_context: "fixture invitation endpoint".into(),
        invitation_secret_context: "fixture invitation secret".into(),
        ticket_prefixes: vec!["fixture1-".into()],
    }
}
pub fn config() -> EndpointConfig {
    EndpointConfig {
        profile: profile(),
        secret_key: SecretKey::generate(),
        relay: RelaySelection::Disabled,
        bind_address: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        direct_paths: true,
        relay_root_certificates: Vec::new(),
    }
}
