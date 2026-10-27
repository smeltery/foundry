mod common;
use foundry_network::signing::{MetadataRecord, SignedRecord};
use iroh::SecretKey;

#[test]
fn prefix_team_boundary_cannot_be_repartitioned() {
    let mut original = common::config().profile;
    original.signing_prefix = "fixture/".into();
    let body = MetadataRecord {
        name: "review".into(),
        relay: None,
        ts: 1,
    };
    let key = SecretKey::from_bytes(&[7; 32]);
    let record = SignedRecord::sign(body.clone(), &key, "abc", &original).unwrap();
    let mut ambiguous = original.clone();
    ambiguous.signing_prefix = "fixture/a".into();
    assert!(record.verify("bc", &ambiguous).is_err());
    assert!(SignedRecord::sign(body, &key, "bc", &ambiguous).is_err());
    record.verify("abc", &original).unwrap();
}

#[test]
fn old_shape_normalization_is_not_raw_omitted_field_signature_support() {
    let profile = common::config().profile;
    let key = SecretKey::from_bytes(&[8; 32]);
    let raw = br#"fixture/signature/1/team-a/meta/{"name":"review","ts":1}"#;
    let signature: String = key
        .sign(raw)
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let record: SignedRecord<MetadataRecord> = serde_json::from_value(serde_json::json!({
        "body": {"name":"review", "ts":1}, "by":key.public(), "sig":signature,
    }))
    .unwrap();
    assert!(record.verify("team-a", &profile).is_err());
    let canonical = SignedRecord::sign(record.body, &key, "team-a", &profile).unwrap();
    let mut value = serde_json::to_value(&canonical).unwrap();
    value["body"].as_object_mut().unwrap().remove("relay");
    value["body"]["unknown"] = serde_json::json!(true);
    let normalized: SignedRecord<MetadataRecord> = serde_json::from_value(value).unwrap();
    normalized.verify("team-a", &profile).unwrap();
}

#[test]
fn malformed_duplicate_fields_and_multibyte_signature_fail_without_panicking() {
    let profile = common::config().profile;
    let key = SecretKey::from_bytes(&[9; 32]);
    let mut record = SignedRecord::sign(
        MetadataRecord {
            name: "review".into(),
            relay: None,
            ts: i64::MIN,
        },
        &key,
        "team-a",
        &profile,
    )
    .unwrap();
    record.verify("team-a", &profile).unwrap();
    record.sig = "é".repeat(64);
    assert!(record.verify("team-a", &profile).is_err());
    assert!(
        serde_json::from_str::<MetadataRecord>(r#"{"name":"a","name":"b","relay":null,"ts":1}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<MetadataRecord>(
            r#"{"name":"a","relay":null,"ts":9223372036854775808}"#
        )
        .is_err()
    );
}
