use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use foundry_network::{
    WireProfile,
    ticket::{MAX_TICKET_BYTES, RelayCompression, Ticket, TicketCodec},
};
use iroh::SecretKey;

const GOLDEN: &str = "f5yt-AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8FRm9yZ2UAAAIEfwAAARI0BgAAAAAAAAAAAAAAAAAAAAEBuztqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop";

fn profile() -> WireProfile {
    WireProfile {
        team_alpn: b"fixture/team/1".to_vec(),
        join_alpn: b"fixture/join/1".to_vec(),
        protocol: 1,
        signing_prefix: "fixture/signature/1/".into(),
        invitation_endpoint_context: "fixture endpoint".into(),
        invitation_secret_context: "fixture secret".into(),
        ticket_prefixes: vec!["f5yt-".into(), "old-f5yt-".into()],
    }
}
fn ticket() -> Ticket {
    Ticket {
        team: HEXLOWER.encode(&(0..16).collect::<Vec<u8>>()),
        secret: HEXLOWER.encode(&(16..32).collect::<Vec<u8>>()),
        name: "Forge".into(),
        relay: None,
        hint_relay: None,
        hint_ips: vec![
            "127.0.0.1:4660".parse().unwrap(),
            "[::1]:443".parse().unwrap(),
        ],
        peers: vec![SecretKey::from_bytes(&[0; 32]).public()],
    }
}
fn wrap(bytes: &[u8]) -> String {
    format!("f5yt-{}", BASE64URL_NOPAD.encode(bytes))
}

#[test]
fn overlapping_prefixes_are_rejected_in_either_order() {
    for prefixes in [["f5y", "f5yt-"], ["f5yt-", "f5y"]] {
        let mut profile = profile();
        profile.ticket_prefixes = prefixes.into_iter().map(str::to_string).collect();
        assert!(TicketCodec::new(&profile, None).is_err());
    }
}

#[test]
fn independent_golden_bytes_and_configured_prefixes_match() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    assert!(codec.encode(&ticket()).unwrap() == GOLDEN);
    assert!(codec.decode(GOLDEN).unwrap() == ticket());
    assert!(
        codec
            .decode(&GOLDEN.replacen("f5yt-", "old-f5yt-", 1))
            .unwrap()
            == ticket()
    );
    assert!(
        codec
            .decode(&format!(" https://example.test/#unused#{GOLDEN} \n"))
            .unwrap()
            == ticket()
    );
    assert!(
        codec
            .decode(&GOLDEN.replacen("f5yt-", "unknown-", 1))
            .is_err()
    );
}

#[test]
fn truncation_and_invalid_payloads_never_echo_invitation_secrets() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let bytes = BASE64URL_NOPAD
        .decode(GOLDEN.strip_prefix("f5yt-").unwrap().as_bytes())
        .unwrap();
    for length in 0..bytes.len() {
        let error = codec.decode(&wrap(&bytes[..length])).err().unwrap();
        assert_eq!(
            error.to_string(),
            "invalid network input: invalid invitation ticket"
        );
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(codec.decode(&wrap(&extra)).is_err());
    let mut family = bytes;
    family[41] = 5;
    assert!(codec.decode(&wrap(&family)).is_err());
    for value in [
        format!("{GOLDEN}="),
        "f5yt-+///".into(),
        wrap(&vec![0; MAX_TICKET_BYTES + 1]),
    ] {
        assert!(codec.decode(&value).is_err());
    }
    let mut invalid = ticket();
    invalid.secret = "not-hex".into();
    assert!(codec.encode(&invalid).is_err());
    invalid.secret = "0".repeat(32);
    invalid.peers = vec![invalid.peers[0]; MAX_TICKET_BYTES / 32 + 1];
    assert!(codec.encode(&invalid).is_err());
}

#[test]
fn text_and_encoder_address_limits_preserve_character_boundaries() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let mut value = ticket();
    value.name = "🦀".repeat(61);
    value.relay = Some("é".repeat(200));
    value.hint_ips.push("192.0.2.1:9".parse().unwrap());
    let decoded = codec.decode(&codec.encode(&value).unwrap()).unwrap();
    assert_eq!(decoded.name, "🦀".repeat(60));
    assert_eq!(decoded.relay, Some("é".repeat(125)));
    assert_eq!(decoded.hint_ips.len(), 2);
}

#[test]
fn exact_payload_limit_roundtrips_and_next_byte_fails() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let mut value = ticket();
    value.name = "n".repeat(28);
    value.hint_ips.clear();
    value.peers = vec![value.peers[0]; (MAX_TICKET_BYTES - 64) / 32];
    let encoded = codec.encode(&value).unwrap();
    assert_eq!(
        BASE64URL_NOPAD
            .decode(encoded.strip_prefix("f5yt-").unwrap().as_bytes())
            .unwrap()
            .len(),
        MAX_TICKET_BYTES
    );
    assert!(codec.decode(&encoded).unwrap() == value);
    value.name.push('n');
    assert!(codec.encode(&value).is_err());
}

#[test]
fn compressed_relays_require_explicit_policy_and_normalize_suffix() {
    let profile = profile();
    let rule = RelayCompression::new(".relay.fixture.test.".into()).unwrap();
    let codec = TicketCodec::new(&profile, Some(&rule)).unwrap();
    let mut value = ticket();
    value.relay = Some("https://zone-a.relay.fixture.test//".into());
    value.hint_relay = Some("https://zone-b.relay.fixture.test./".into());
    let encoded = codec.encode(&value).unwrap();
    assert!(
        TicketCodec::new(&profile, None)
            .unwrap()
            .decode(&encoded)
            .is_err()
    );
    let decoded = codec.decode(&encoded).unwrap();
    assert_eq!(
        decoded.relay.as_deref(),
        Some("https://zone-a.relay.fixture.test./")
    );
    assert_eq!(decoded.hint_relay, value.hint_relay);
    value.relay = Some(format!("https://{}.relay.fixture.test/", "x".repeat(32)));
    let plain = codec.encode(&value).unwrap();
    assert_eq!(codec.decode(&plain).unwrap().relay, value.relay);
    assert!(RelayCompression::new("https://invalid/".into()).is_err());
}

#[test]
fn decoder_preserves_lossy_text_marker_semantics_and_all_encoded_addresses() {
    let profile = profile();
    let rule = RelayCompression::new(".relay.fixture.test.".into()).unwrap();
    let codec = TicketCodec::new(&profile, Some(&rule)).unwrap();
    for (name, expected) in [
        (vec![1, 0xfe], "�"),
        (vec![255, 1, b'x'], "https://x.relay.fixture.test./"),
    ] {
        let mut bytes = vec![0; 32];
        bytes.extend(name);
        bytes.extend([0, 0, 3]);
        for suffix in 1..=3 {
            bytes.extend([4, 192, 0, 2, suffix, 0, 9]);
        }
        bytes.extend(ticket().peers[0].as_bytes());
        let decoded = codec.decode(&wrap(&bytes)).unwrap();
        assert_eq!(decoded.name, expected);
        assert_eq!(decoded.hint_ips.len(), 3);
        // Only encoding truncates hints to two; accepting three is wire-compatible.
        assert_eq!(
            codec
                .decode(&codec.encode(&decoded).unwrap())
                .unwrap()
                .hint_ips
                .len(),
            2
        );
    }
}

#[test]
fn addresses_apply_team_relay_to_all_peers_and_hints_only_to_first() {
    let mut value = ticket();
    value.peers.push(SecretKey::from_bytes(&[1; 32]).public());
    value.hint_relay = Some("https://hint.example.test/".into());
    let addresses = value.peer_addresses();
    assert_eq!(addresses[0].ip_addrs().count(), 2);
    assert_eq!(addresses[1].ip_addrs().count(), 0);
    assert_eq!(addresses[0].relay_urls().count(), 1);
    assert_eq!(addresses[1].relay_urls().count(), 0);
    value.relay = Some("https://team.example.test/".into());
    for address in value.peer_addresses() {
        assert_eq!(
            address.relay_urls().next().unwrap().as_str(),
            "https://team.example.test/"
        );
    }
    value.relay = Some("invalid url".into());
    assert!(
        value
            .peer_addresses()
            .iter()
            .all(|address| address.relay_urls().next().is_none())
    );
}
