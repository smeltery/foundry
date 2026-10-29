use foundry_contracts::adapter_selection::AdapterSelection;
use serde_json::{Value, json};

fn selection() -> Value {
    json!({
        "provider":"fixture", "model":"small", "effort":"high",
        "directory":"/workspace/α", "workspace_identity":"11".repeat(32),
        "profile":"explicit-core-v1", "profile_version":1,
        "permission_profile":"current", "resource_policy":"explicit_system_v1",
        "resource_identity":"22".repeat(32), "tool_catalog":"core_workspace_v1",
        "tools":["list","read","write"], "policy_identity":"33".repeat(32),
        "generation":7, "max_output_bytes":1048576, "max_pending_text_bytes":65536,
    })
}

#[test]
fn public_selection_matches_independent_length_framed_utf8_golden() {
    let value: AdapterSelection = serde_json::from_value(selection()).unwrap();
    assert_eq!(
        value.digest().unwrap().to_hex(),
        "eded5f559050b77112de97583207b48bff9b351df85afe6671aecdb3c123d794"
    );
    let pretty = serde_json::to_string_pretty(&selection()).unwrap();
    let reordered: AdapterSelection = serde_json::from_str(&pretty).unwrap();
    assert!(value.digest().unwrap() == reordered.digest().unwrap());
}

#[test]
fn every_mutable_selection_field_changes_the_identity() {
    let baseline: AdapterSelection = serde_json::from_value(selection()).unwrap();
    for (key, replacement) in [
        ("provider", json!("other")),
        ("model", json!("large")),
        ("effort", json!("off")),
        ("directory", json!("/different")),
        ("workspace_identity", json!("44".repeat(32))),
        ("profile", json!("explicit-core-v2")),
        ("profile_version", json!(2)),
        ("permission_profile", json!("legacy")),
        ("resource_identity", json!("44".repeat(32))),
        ("tools", json!(["read"])),
        ("policy_identity", json!("44".repeat(32))),
        ("generation", json!(8)),
        ("max_output_bytes", json!(4096)),
        ("max_pending_text_bytes", json!(4096)),
    ] {
        let mut value = selection();
        value[key] = replacement;
        let changed: AdapterSelection = serde_json::from_value(value).unwrap();
        assert!(
            changed.digest().unwrap() != baseline.digest().unwrap(),
            "{key}"
        );
    }
}

#[test]
fn unsupported_or_unbounded_selection_never_gets_an_identity() {
    for (key, replacement) in [
        ("provider", json!(" ")),
        ("provider", json!("x".repeat(129))),
        ("model", json!("x".repeat(257))),
        ("model", json!("bad\0model")),
        ("effort", json!("automatic")),
        ("directory", json!("relative")),
        ("directory", json!(format!("/{}", "x".repeat(4096)))),
        ("profile", json!("")),
        ("profile_version", json!(0)),
        ("permission_profile", json!("unknown")),
        ("resource_policy", json!("ambient")),
        ("tool_catalog", json!("automatic")),
        ("tools", json!(["read", "list"])),
        ("tools", json!(["read", "read"])),
        ("tools", json!(["shell"])),
        ("generation", json!(0)),
        ("generation", json!(9_007_199_254_740_992_u64)),
        ("max_output_bytes", json!(0)),
        ("max_pending_text_bytes", json!(1_048_577)),
    ] {
        let mut value = selection();
        value[key] = replacement;
        let invalid: AdapterSelection = serde_json::from_value(value).unwrap();
        assert!(invalid.digest().is_err(), "{key}");
    }
}

#[test]
fn decoding_rejects_unknown_private_duplicate_and_invalid_digest_fields() {
    for (key, value) in [
        ("credential", json!("private-canary")),
        ("endpoint", json!("https://invalid.example")),
        ("workspace_identity", json!("AB".repeat(32))),
        ("profile_version", json!(-1)),
        ("generation", json!(1.5)),
    ] {
        let mut raw = selection();
        raw[key] = value;
        assert!(
            serde_json::from_value::<AdapterSelection>(raw).is_err(),
            "{key}"
        );
    }
    let raw = serde_json::to_string(&selection()).unwrap();
    let duplicate = raw.replacen("{", "{\"generation\":7,", 1);
    assert!(serde_json::from_str::<AdapterSelection>(&duplicate).is_err());
}
