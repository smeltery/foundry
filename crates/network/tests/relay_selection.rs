//! A configured relay list is accepted whole or rejected whole.
use foundry_network::{
    RelaySelection,
    daemon_config::{ConfigError, TransportConfig},
};
use serde_json::{Value, json};

fn transport(relays: Value, direct_paths: bool) -> TransportConfig {
    serde_json::from_value(
        json!({"bind_address":"127.0.0.1:0","direct_paths":direct_paths,
        "relays":relays,"relay_root_certificates":[]}),
    )
    .unwrap()
}
fn relays(count: usize) -> Vec<String> {
    (0..count)
        .map(|n| format!("https://relay-{n}.example.test"))
        .collect()
}
fn selected(config: &TransportConfig) -> Vec<String> {
    match config.relay_selection().unwrap() {
        RelaySelection::Custom(urls) => urls.iter().map(ToString::to_string).collect(),
        RelaySelection::Disabled => Vec::new(),
    }
}

#[test]
fn sixteen_relays_are_kept_in_order_and_seventeen_are_refused() {
    let list = relays(16);
    let chosen = selected(&transport(json!(list), false));
    assert_eq!(chosen.len(), 16);
    for (url, configured) in chosen.iter().zip(&list) {
        assert!(url.starts_with(configured.as_str()), "{url} {configured}");
    }
    for direct_paths in [true, false] {
        assert_eq!(
            transport(json!(relays(17)), direct_paths)
                .relay_selection()
                .unwrap_err(),
            ConfigError::Invalid
        );
    }
}

#[test]
fn no_relays_means_direct_only_and_is_refused_without_direct_paths() {
    assert!(matches!(
        transport(json!([]), true).relay_selection().unwrap(),
        RelaySelection::Disabled
    ));
    assert_eq!(
        transport(json!([]), false).relay_selection().unwrap_err(),
        ConfigError::Invalid
    );
}

#[test]
fn one_faulty_entry_rejects_the_entire_list() {
    let faults = [
        "not a url",
        "relay.example.test",
        "https://",
        "ftp://relay.example.test",
        "http://relay.example.test",
        "https://name@relay.example.test",
        "https://relay.example.test/?token=1",
        "https://relay.example.test/#part",
        "https://relay-3.example.test",
    ];
    for fault in faults {
        for position in [0, 3, 6] {
            let mut list: Vec<Value> = relays(6).into_iter().map(Value::from).collect();
            list.insert(position, json!(fault));
            assert_eq!(
                transport(Value::Array(list), true)
                    .relay_selection()
                    .unwrap_err(),
                ConfigError::Invalid,
                "{fault} at {position}"
            );
        }
    }
}

#[test]
fn malformed_relay_list_shapes_fail_to_parse() {
    for relays in [json!("https://relay.example.test"), json!([7]), json!(null)] {
        let value = json!({"bind_address":"127.0.0.1:0","direct_paths":true,
            "relays":relays,"relay_root_certificates":[]});
        assert!(serde_json::from_value::<TransportConfig>(value).is_err());
    }
}
