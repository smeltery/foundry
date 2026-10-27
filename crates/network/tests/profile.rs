mod common;
use common::{config, profile};
use foundry_network::{Channel, NetworkEndpoint, RelaySelection, WireProfile};

#[test]
fn explicit_profile_is_required_and_channel_selection_is_exact() {
    let valid = profile();
    valid.validate().unwrap();
    assert_eq!(valid.alpn(Channel::Team), b"fixture/team/1");
    assert_eq!(valid.alpn(Channel::Join), b"fixture/join/1");
    assert!(serde_json::from_str::<WireProfile>("{}").is_err());
    let mut encoded = serde_json::to_value(valid).unwrap();
    encoded["unknown"] = true.into();
    assert!(serde_json::from_value::<WireProfile>(encoded).is_err());
}

#[test]
fn rejects_invalid_alpns_contexts_and_prefixes() {
    for index in 0..10 {
        let mut invalid = profile();
        match index {
            0 => invalid.team_alpn.clear(),
            1 => invalid.join_alpn = vec![b'x'; 256],
            2 => invalid.join_alpn = invalid.team_alpn.clone(),
            3 => invalid.protocol = 0,
            4 => invalid.signing_prefix = " ".into(),
            5 => invalid.invitation_endpoint_context = "line\nbreak".into(),
            6 => invalid.invitation_secret_context = invalid.invitation_endpoint_context.clone(),
            7 => invalid.ticket_prefixes.clear(),
            8 => invalid
                .ticket_prefixes
                .push(invalid.ticket_prefixes[0].clone()),
            _ => invalid.ticket_prefixes[0] = "bad prefix".into(),
        }
        assert!(invalid.validate().is_err(), "case {index}");
    }
}

#[tokio::test]
async fn invalid_endpoint_settings_fail_before_binding() {
    for index in 0..6 {
        let mut invalid = config();
        match index {
            0 => invalid.direct_paths = false,
            1 => invalid.relay = RelaySelection::Custom(vec![]),
            2 => invalid.relay = RelaySelection::Custom(vec!["http://127.0.0.1".parse().unwrap()]),
            3 => {
                invalid.relay =
                    RelaySelection::Custom(vec!["https://user:secret@127.0.0.1".parse().unwrap()])
            }
            4 => invalid.relay_root_certificates = vec![vec![]],
            _ => {
                invalid.relay =
                    RelaySelection::Custom(vec!["https://127.0.0.1".parse().unwrap(); 2])
            }
        }
        assert!(
            NetworkEndpoint::bind(invalid).await.is_err(),
            "case {index}"
        );
    }
}
