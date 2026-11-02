#![cfg(unix)]
#[path = "daemon_config_network/inputs.rs"]
mod inputs;
#[path = "daemon_config_network/startup.rs"]
mod startup;
#[path = "daemon_config_network/support.rs"]
mod support;
use foundry_network::{
    RelaySelection,
    daemon_config::{ConfigError, DaemonConfig, NetworkMode},
};
use serde_json::json;
use std::fs;
use support::*;

#[test]
fn disabled_and_explicit_direct_configuration_have_no_startup_side_effects() {
    let root = Root::new();
    assert!(matches!(
        root.load(json!("disabled")).network,
        NetworkMode::Disabled
    ));
    let value = network();
    let config = root.load(value);
    let before = fs::read_dir(&root.0).unwrap().count();
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    assert_eq!(resolved.profile.protocol, 73);
    let key = iroh::SecretKey::generate();
    let endpoint = resolved.endpoint_config(key.clone());
    assert_eq!(endpoint.secret_key.public(), key.public());
    assert!(endpoint.direct_paths);
    assert_eq!(endpoint.bind_address.to_string(), "127.0.0.1:0");
    assert!(matches!(endpoint.relay, RelaySelection::Disabled));
    assert!(endpoint.relay_root_certificates.is_empty());
    assert!(resolved.seeds.is_empty());
    assert!(resolved.code_policy().unwrap().is_none());
    resolved.store_policy().unwrap();
    assert_eq!(before, fs::read_dir(&root.0).unwrap().count());
    assert!(!config.state_directory.exists());
}

#[test]
fn explicit_seed_addresses_and_relay_only_mode_preserve_caller_choices() {
    let root = Root::new();
    let peer = iroh::SecretKey::generate().public();
    let mut value = network();
    value["transport"]["direct_paths"] = json!(false);
    value["transport"]["relays"] = json!(["https://127.0.0.1:4567/"]);
    value["seeds"] = json!([{"endpoint":peer,"direct":["127.0.0.1:3456","[::1]:3457"],"relay":"https://relay.invalid/"}]);
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let mut expected = iroh::EndpointAddr::new(peer)
        .with_ip_addr("127.0.0.1:3456".parse().unwrap())
        .with_ip_addr("[::1]:3457".parse().unwrap());
    expected = expected.with_relay_url("https://relay.invalid/".parse().unwrap());
    assert_eq!(resolved.seeds, vec![expected]);
    let endpoint = resolved.endpoint_config(iroh::SecretKey::generate());
    assert!(!endpoint.direct_paths);
    match endpoint.relay {
        RelaySelection::Custom(urls) => assert_eq!(urls[0].to_string(), "https://127.0.0.1:4567/"),
        RelaySelection::Disabled => panic!("explicit relay lost"),
    }
}

#[test]
fn invalid_transport_profile_and_nested_unknown_fields_are_closed_and_redacted() {
    let root = Root::new();
    let mut cases = vec![json!({"disabled":null}), json!({"mode":"disabled"})];
    for pointer in ["/", "/profile/", "/transport/", "/trust/", "/startup/"] {
        let mut value = network();
        value.pointer_mut(pointer.trim_end_matches('/')).unwrap()["unexpected-secret-marker"] =
            json!("bearer-marker");
        cases.push(value);
    }
    for relay in [
        "http://relay.invalid",
        "https://user:bearer-marker@relay.invalid",
        "https://relay.invalid/?x=1",
        "https://relay.invalid/#x",
    ] {
        let mut value = network();
        value["transport"]["relays"] = json!([relay]);
        cases.push(value);
    }
    for (pointer, entry) in [
        ("/transport/direct_paths", json!(false)),
        ("/transport/bind_address", json!("not-a-socket")),
        (
            "/transport/relays",
            json!(["https://relay.invalid", "https://relay.invalid/"]),
        ),
        ("/profile/protocol", json!(0)),
        ("/profile/signing_prefix", json!("missing-separator")),
        ("/expected_team", json!("")),
        ("/profile/join_alpn", json!([102, 120, 116])),
        ("/profile/team_alpn", json!([])),
        ("/profile/ticket_prefixes", json!(["prefix", "prefixlong"])),
        ("/ticket_relay_compression", json!("invalid-suffix")),
        (
            "/startup",
            json!({"mode":"join_code","code":"bearer-marker"}),
        ),
        ("/codebook", json!({"path":"book","sha256":"bearer-marker"})),
        (
            "/seeds",
            json!([{"endpoint":"bearer-marker","direct":[],"relay":null}]),
        ),
        (
            "/seeds",
            json!([{"endpoint":iroh::SecretKey::generate().public(),"direct":[],"relay":null,"extra":true}]),
        ),
    ] {
        let mut value = network();
        *value.pointer_mut(pointer).unwrap() = entry;
        cases.push(value);
    }
    for (index, value) in cases.into_iter().enumerate() {
        let path = root.write(
            "daemon.json",
            &serde_json::to_vec(&root.config(value)).unwrap(),
        );
        let error = match DaemonConfig::load(&path) {
            Ok(_) => panic!("invalid network case {index} accepted"),
            Err(e) => e,
        };
        assert_eq!(error, ConfigError::Invalid);
        assert!(!format!("{error:?} {error}").contains("bearer-marker"));
    }
    let value = root.config(json!({"mode":"bearer-marker"}));
    let error = serde_json::from_value::<DaemonConfig>(value).err().unwrap();
    assert!(!error.to_string().contains("bearer-marker"));
}

#[test]
fn exact_main_config_byte_ceiling_and_large_protocol_number_remain_usable() {
    let root = Root::new();
    let mut value = network();
    value["profile"]["protocol"] = json!(u32::MAX);
    let mut bytes = serde_json::to_vec(&root.config(value)).unwrap();
    bytes.resize(65536, b' ');
    let path = root.write("daemon.json", &bytes);
    let config = DaemonConfig::load(&path).unwrap();
    assert_eq!(
        enabled(&config).resolve(&root.0).unwrap().profile.protocol,
        u32::MAX
    );
    bytes.push(b' ');
    root.write("daemon.json", &bytes);
    assert!(matches!(
        DaemonConfig::load(&path),
        Err(ConfigError::Invalid)
    ));
}
