use super::support::*;
use foundry_network::{
    WireProfile,
    daemon_config::{ConfigError, ResolvedStartup},
    ticket::{Ticket, TicketCodec},
};
use serde_json::json;

fn invitation() -> (Ticket, String) {
    let ticket = Ticket {
        team: "ab".repeat(16),
        secret: "cd".repeat(16),
        name: "original fixture".into(),
        relay: None,
        hint_relay: Some("https://relay.invalid/".into()),
        hint_ips: vec!["127.0.0.1:5432".parse().unwrap()],
        peers: vec![iroh::SecretKey::generate().public()],
    };
    let profile: WireProfile = serde_json::from_value(profile()).unwrap();
    let text = TicketCodec::new(&profile, None)
        .unwrap()
        .encode(&ticket)
        .unwrap();
    (ticket, text)
}

#[test]
fn resolved_ticket_codec_keeps_compression_for_later_controls() {
    let root = Root::new();
    let mut settings = network();
    settings["ticket_relay_compression"] = json!(".relay.fixture.");
    let config = root.load(settings);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let (mut ticket, _) = invitation();
    ticket.relay = Some("https://west.relay.fixture./".into());
    ticket.hint_relay = None;
    let expected_compression =
        foundry_network::ticket::RelayCompression::new(".relay.fixture.".into()).unwrap();
    let expected = TicketCodec::new(&resolved.profile, Some(&expected_compression))
        .unwrap()
        .encode(&ticket)
        .unwrap();
    let codec = resolved.ticket_codec().unwrap();
    assert_eq!(codec.encode(&ticket).unwrap(), expected);
    assert!(codec.decode(&expected).unwrap() == ticket);
    assert!(
        TicketCodec::new(&resolved.profile, None)
            .unwrap()
            .decode(&expected)
            .is_err()
    );
}

#[test]
fn formation_and_existing_modes_are_explicit_and_keep_optional_inputs() {
    let root = Root::new();
    let mut value = network();
    value["startup"] = json!({"mode":"form","name":"local team with spaces",
        "relay":"https://relay.invalid/","proof":"secret-proof-marker"});
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    match resolved.startup {
        ResolvedStartup::Form { name, relay, proof } => {
            assert_eq!(name, "local team with spaces");
            assert_eq!(relay.as_deref(), Some("https://relay.invalid/"));
            assert_eq!(proof.as_deref(), Some("secret-proof-marker"));
        }
        _ => panic!("wrong startup mode"),
    }
    let config = root.load(network());
    assert!(matches!(
        enabled(&config).resolve(&root.0).unwrap().startup,
        ResolvedStartup::Existing
    ));
}

#[test]
fn ticket_decoding_preserves_hints_secret_and_optional_founder_without_forcing_a_pin() {
    let root = Root::new();
    let (expected, text) = invitation();
    for founder in [None, Some(iroh::SecretKey::generate().public())] {
        let mut value = network();
        value["startup"] = json!({"mode":"join_ticket","ticket":format!("https://invitation.invalid/#{}",text),
            "expected_founder":founder,"owner_only":true});
        let config = root.load(value);
        match enabled(&config).resolve(&root.0).unwrap().startup {
            ResolvedStartup::JoinTicket {
                ticket,
                expected_founder,
                owner_only,
                proof,
            } => {
                assert!(ticket == expected);
                assert_eq!(expected_founder, founder);
                assert!(owner_only);
                assert!(proof.is_none());
            }
            _ => panic!("wrong startup mode"),
        }
    }
}

#[test]
fn code_mode_uses_actual_normalization_without_requiring_independent_team_or_founder_pins() {
    let root = Root::new();
    let mut value = network();
    value["codebook"] = codebook(&root);
    value["startup"] = json!({"mode":"join_code","code":"BIRCH; cobalt / dahlia ELM"});
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let policy = resolved.code_policy().unwrap().unwrap();
    match resolved.startup {
        ResolvedStartup::JoinCode {
            code,
            expected_team,
            expected_founder,
            owner_only,
            proof,
        } => {
            assert_eq!(code.expose(), "birch-cobalt-dahlia-elm");
            assert!(
                expected_team.is_none()
                    && expected_founder.is_none()
                    && !owner_only
                    && proof.is_none()
            );
            let actual = policy.derive(&code).unwrap();
            assert!(
                policy
                    .authenticate_endpoint(&code, actual.endpoint_key().public())
                    .is_ok()
            );
            assert!(
                policy
                    .authenticate_endpoint(&code, iroh::SecretKey::generate().public())
                    .is_err()
            );
        }
        _ => panic!("wrong startup mode"),
    }
}

#[test]
fn referenced_startup_secrets_are_loaded_explicitly_and_fail_without_echoing_values() {
    let root = Root::new();
    let (expected, text) = invitation();
    root.write("invitation.txt", text.as_bytes());
    root.write("proof.txt", b"private-proof-marker");
    let mut value = network();
    value["startup"] = json!({"mode":"join_ticket",
        "ticket":{"path":"invitation.txt"},"proof":{"path":"proof.txt"}});
    let config = root.load(value);
    match enabled(&config).resolve(&root.0).unwrap().startup {
        ResolvedStartup::JoinTicket { ticket, proof, .. } => {
            assert!(ticket == expected);
            assert_eq!(proof.as_deref(), Some("private-proof-marker"));
        }
        _ => panic!("wrong startup mode"),
    }
    root.write("invitation.txt", b"malformed-bearer-marker");
    let error = enabled(&config).resolve(&root.0).err().unwrap();
    assert_eq!(error, ConfigError::Invalid);
    assert!(!format!("{error:?} {error}").contains("bearer-marker"));
    root.write("invitation.txt", &vec![b'a'; 262145]);
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
    let mut value = network();
    value["codebook"] = codebook(&root);
    value["startup"] = json!({"mode":"join_code","code":{"path":"code.txt"}});
    root.write("code.txt", b"birch cobalt dahlia elm");
    let config = root.load(value);
    assert!(matches!(
        enabled(&config).resolve(&root.0).unwrap().startup,
        ResolvedStartup::JoinCode { .. }
    ));
    root.write("code.txt", &vec![b'a'; 4097]);
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
}

#[test]
fn referenced_wire_ticket_can_exceed_main_config_ceiling_without_truncating_peers() {
    let root = Root::new();
    let (mut expected, _) = invitation();
    expected.peers = vec![expected.peers[0]; 1600];
    let profile: WireProfile = serde_json::from_value(profile()).unwrap();
    let text = TicketCodec::new(&profile, None)
        .unwrap()
        .encode(&expected)
        .unwrap();
    assert!(text.len() > 65536);
    root.write("large-invitation.txt", text.as_bytes());
    let mut value = network();
    value["startup"] = json!({"mode":"join_ticket","ticket":{"path":"large-invitation.txt"}});
    let config = root.load(value);
    match enabled(&config).resolve(&root.0).unwrap().startup {
        ResolvedStartup::JoinTicket { ticket, .. } => assert!(ticket == expected),
        _ => panic!("wrong startup mode"),
    }
}

#[test]
fn existing_configuration_leaves_a_real_empty_protected_store_unjoined() {
    use foundry_network::{
        full_team_store::{FullTeamStore, LoadedStatus},
        identity::load_or_create_identity,
    };
    use std::{sync::Arc, time::Duration};
    let root = Root::new();
    let config = root.load(network());
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    std::fs::create_dir(&config.state_directory).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        &config.state_directory,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let dir =
        cap_std::fs::Dir::open_ambient_dir(&config.state_directory, cap_std::ambient_authority())
            .unwrap();
    let identity = Arc::new(load_or_create_identity(&dir, Duration::from_secs(1)).unwrap());
    let mut store = FullTeamStore::create_new(
        &config.state_directory,
        identity.clone(),
        resolved.store_policy().unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    let empty = store.load(1000).unwrap();
    assert_eq!(empty.revision(), 0);
    assert!(empty.document().is_none());
    drop(store);
    let again = enabled(&config).resolve(&root.0).unwrap();
    assert!(matches!(again.startup, ResolvedStartup::Existing));
    let mut store = FullTeamStore::open_existing(
        &config.state_directory,
        identity,
        again.store_policy().unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(matches!(
        store.load_status(1000).unwrap(),
        LoadedStatus::Empty(_)
    ));
    assert_eq!(store.load(1000).unwrap().revision(), 0);
}

#[test]
fn independent_common_and_code_team_pins_are_preserved_for_runtime_authority_checks() {
    let root = Root::new();
    for startup in [
        json!({"mode":"existing"}),
        json!({"mode":"form","name":"team"}),
        json!({"mode":"join_code","code":"birch cobalt dahlia elm","expected_team":"independent-team-pin"}),
    ] {
        let mut value = network();
        value["codebook"] = codebook(&root);
        value["expected_team"] = json!("independent-team-pin");
        value["startup"] = startup;
        let config = root.load(value);
        let resolved = enabled(&config).resolve(&root.0).unwrap();
        assert_eq!(
            resolved.expected_team.as_deref(),
            Some("independent-team-pin")
        );
        if let ResolvedStartup::JoinCode { expected_team, .. } = resolved.startup {
            assert_eq!(expected_team.as_deref(), Some("independent-team-pin"));
        }
    }
}
