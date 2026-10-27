mod common;
fn profile() -> foundry_network::WireProfile {
    common::config().profile
}
use foundry_network::signing::{MemberRecord, MetadataRecord, SignedRecord, signing_payload};
use iroh::SecretKey;

fn metadata() -> MetadataRecord {
    MetadataRecord {
        name: "sample".into(),
        relay: None,
        ts: 17,
    }
}

#[test]
fn typed_metadata_golden_order_null_and_escaping() {
    let expected =
        b"fixture/signature/1/team-a/meta/{\"name\":\"sample\",\"relay\":null,\"ts\":17}";
    assert_eq!(
        signing_payload(&profile(), "team-a", &metadata()).unwrap(),
        expected
    );
    let body = MetadataRecord {
        name: "a\n\"é".into(),
        relay: Some("https://relay.invalid".into()),
        ts: -1,
    };
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        "{\"name\":\"a\\n\\\"é\",\"relay\":\"https://relay.invalid\",\"ts\":-1}"
    );
}

#[test]
fn typed_member_golden_preserves_false_and_field_order() {
    let key = SecretKey::from_bytes(&[0; 32]);
    let body = MemberRecord {
        endpoint: key.public(),
        person: "ada".into(),
        machine: "desk".into(),
        active: false,
        ts: 42,
    };
    let expected = concat!(
        "fixture/signature/1/team-a/member/{\"endpoint\":\"",
        "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
        "\",\"person\":\"ada\",\"machine\":\"desk\",\"active\":false,\"ts\":42}"
    );
    assert_eq!(
        signing_payload(&profile(), "team-a", &body).unwrap(),
        expected.as_bytes()
    );
}

#[test]
fn real_key_signature_roundtrip_and_every_metadata_field_is_covered() {
    let key = SecretKey::generate();
    let signed = SignedRecord::sign(metadata(), &key, "team-a", &profile()).unwrap();
    let encoded = serde_json::to_vec(&signed).unwrap();
    let decoded: SignedRecord<MetadataRecord> = serde_json::from_slice(&encoded).unwrap();
    decoded.verify("team-a", &profile()).unwrap();
    assert_eq!(signed, decoded);
    let mut changes = vec![signed.clone(); 3];
    changes[0].body.name.push('x');
    changes[1].body.relay = Some("https://other.invalid".into());
    changes[2].body.ts += 1;
    for changed in changes {
        assert!(changed.verify("team-a", &profile()).is_err());
    }
    assert!(signed.verify("team-b", &profile()).is_err());
    let mut other = profile();
    other.signing_prefix.push('x');
    assert!(signed.verify("team-a", &other).is_err());
}

#[test]
fn forged_signer_signature_encoding_and_reordered_json_are_rejected() {
    let key = SecretKey::generate();
    let original = SignedRecord::sign(metadata(), &key, "team-a", &profile()).unwrap();
    let mut changed = original.clone();
    changed.by = SecretKey::generate().public();
    assert!(changed.verify("team-a", &profile()).is_err());
    for sig in [
        "ff".repeat(64),
        "00".repeat(63),
        "GG".repeat(64),
        original.sig.to_uppercase(),
    ] {
        let mut changed = original.clone();
        changed.sig = sig;
        assert!(changed.verify("team-a", &profile()).is_err());
    }
    let unordered =
        b"fixture/signature/1/team-a/meta/{\"ts\":17,\"relay\":null,\"name\":\"sample\"}";
    let mut changed = original;
    changed.sig = key
        .sign(unordered)
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(changed.verify("team-a", &profile()).is_err());
}

#[test]
fn metadata_missing_optional_relay_normalizes_to_documented_null_shape() {
    let body: MetadataRecord =
        serde_json::from_str(r#"{"ts":17,"name":"sample","extra":true}"#).unwrap();
    assert_eq!(body, metadata());
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"name":"sample","relay":null,"ts":17}"#
    );
    assert!(serde_json::from_str::<MetadataRecord>(r#"{"name":"sample"}"#).is_err());
}

#[test]
fn every_member_field_is_signed_and_missing_required_fields_fail() {
    let key = SecretKey::generate();
    let signed = SignedRecord::sign(
        MemberRecord {
            endpoint: key.public(),
            person: "a".into(),
            machine: "b".into(),
            active: true,
            ts: 1,
        },
        &key,
        "team-a",
        &profile(),
    )
    .unwrap();
    let mut changes = vec![signed.clone(); 5];
    changes[0].body.endpoint = SecretKey::generate().public();
    changes[1].body.person.push('x');
    changes[2].body.machine.push('x');
    changes[3].body.active = false;
    changes[4].body.ts += 1;
    for changed in changes {
        assert!(changed.verify("team-a", &profile()).is_err());
    }
    let mut value = serde_json::to_value(&signed.body).unwrap();
    value.as_object_mut().unwrap().remove("active");
    assert!(serde_json::from_value::<MemberRecord>(value).is_err());
}

#[test]
fn independent_ed25519_golden_signatures_match_typed_payloads() {
    let key = SecretKey::from_bytes(&[0; 32]);
    let metadata = SignedRecord::sign(metadata(), &key, "team-a", &profile()).unwrap();
    assert_eq!(
        metadata.sig,
        concat!(
            "abe2d46621287c629d150066034ff2a32cba795bcf08a3bd914409654c030139bb",
            "252d13c796bc76a9ae17b9abe7a625349764cf30b8216e81e328cdc6013501"
        )
    );
    let mut member = SignedRecord::sign(
        MemberRecord {
            endpoint: key.public(),
            person: "ada".into(),
            machine: "desk".into(),
            active: false,
            ts: 42,
        },
        &key,
        "team-a",
        &profile(),
    )
    .unwrap();
    assert_eq!(
        member.sig,
        concat!(
            "f934681c64ed17afd2369c553e1849d1509e7b704616d7d048b2319201962e513e",
            "a050dd4a92d96895cb4482771c21fac2c868077bd5720222ff119f0a96560e"
        )
    );
    member.verify("team-a", &profile()).unwrap();
    let sorted = serde_json::to_value(&member.body).unwrap();
    let mut payload = b"fixture/signature/1/team-a/member/".to_vec();
    payload.extend(serde_json::to_vec(&sorted).unwrap());
    assert_ne!(
        payload,
        signing_payload(&profile(), "team-a", &member.body).unwrap()
    );
    member.sig = key
        .sign(&payload)
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(member.verify("team-a", &profile()).is_err());
}
