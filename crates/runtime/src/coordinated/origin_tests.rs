use super::*;
use serde_json::json;
fn data() -> serde_json::Value {
    let hash = "01".repeat(32);
    json!({"origin":"restored_baseline","identity":{
        "session_id":"s","checkpoint_id":"checkpoint","generation":1,
        "source_attempt_id":"capture-attempt","original_prompt_attempt_id":"first-attempt",
        "prompt_digest":hash,"prompt_bytes":4,"record_digest":hash,"payload_digest":hash,
        "witness_digest":hash,"witness_bytes":100,"history_binding":hash,"prefix_binding":hash,
        "logical_records":3,"prefix_records":10,"mapping_refs":4,"profile_binding":hash}})
}
fn decode(value: &serde_json::Value) -> Result<OriginIdentity, CaptureError> {
    OriginIdentity::decode(&serde_json::to_vec(value).unwrap())
}
#[test]
fn historical_prompt_and_origin_cannot_be_relabelled() {
    let baseline = decode(&data()).unwrap();
    assert!(baseline.is_restored());
    let digest = ContextDigest::parse(&"01".repeat(32)).unwrap();
    baseline
        .check_prompt("s", "first-attempt", digest, 4)
        .unwrap();
    assert!(
        baseline
            .check_prompt("s", "capture-attempt", digest, 4)
            .is_err()
    );
    assert!(
        baseline
            .check_prompt("s", "new-attempt", digest, 4)
            .is_err()
    );
    let binding = baseline.binding_digest().unwrap();
    for field in [
        "source_attempt_id",
        "original_prompt_attempt_id",
        "checkpoint_id",
    ] {
        let mut changed = data();
        changed["identity"][field] = json!("new-attempt");
        assert!(decode(&changed).unwrap().check_binding(binding).is_err());
    }
    for field in [
        "history_binding",
        "prefix_binding",
        "profile_binding",
        "witness_digest",
        "record_digest",
        "payload_digest",
    ] {
        let mut changed = data();
        changed["identity"][field] = json!("02".repeat(32));
        assert!(decode(&changed).unwrap().check_binding(binding).is_err());
    }
    let fresh = OriginIdentity::fresh(FreshOpeningIdentity {
        session_id: ContextId::new("s").unwrap(),
        delivery_attempt_id: ContextId::new("first-attempt").unwrap(),
        prompt_digest: digest,
        prompt_bytes: 4,
    })
    .unwrap();
    assert!(fresh.check_binding(binding).is_err());
    let roundtrip = OriginIdentity::decode(&baseline.encode().unwrap()).unwrap();
    roundtrip.check_binding(binding).unwrap();
}
#[test]
fn identity_counts_and_utf8_ids_have_exact_limits() {
    for (field, max) in [
        ("witness_bytes", MAX_WITNESS_BYTES),
        ("logical_records", MAX_LOGICAL_RECORDS),
        ("prefix_records", MAX_PREFIX_RECORDS),
        ("mapping_refs", MAX_MAPPING_REFS),
        ("prompt_bytes", MAX_PROMPT_BYTES as u64),
        ("generation", i64::MAX as u64),
    ] {
        for (value, accepted) in [(0, false), (max, true), (max + 1, false)] {
            let mut input = data();
            input["identity"][field] = json!(value);
            assert_eq!(decode(&input).is_ok(), accepted, "{field}:{value}");
        }
    }
    for (text, accepted) in [
        ("é".repeat(64), true),
        ("é".repeat(65), false),
        ("bad\n".into(), false),
    ] {
        let mut input = data();
        input["identity"]["session_id"] = json!(text);
        assert_eq!(decode(&input).is_ok(), accepted);
    }
    assert!(OriginIdentity::decode(&vec![b' '; MAX_ORIGIN_BYTES + 1]).is_err());
}
#[test]
fn nested_duplicate_unknown_and_wrong_origin_fields_are_rejected() {
    let original = serde_json::to_string(&data()).unwrap();
    for changed in [
        original.replace("\"generation\":1", "\"generation\":1,\"generation\":1"),
        original.replace(
            "\"identity\":{",
            "\"identity\":{\"attempt_id\":\"fabricated\",",
        ),
        original.replace(
            "\"origin\":\"restored_baseline\"",
            "\"origin\":\"fresh_opening\"",
        ),
        original.replace(
            "\"origin\":",
            "\"origin\":\"restored_baseline\",\"origin\":",
        ),
    ] {
        assert!(OriginIdentity::decode(changed.as_bytes()).is_err());
    }
}
