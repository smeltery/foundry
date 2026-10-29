use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

fn installation() -> Value {
    json!({
        "version":1,"executable":"/usr/bin/true",
        "stored":{"format_id":"fixture","format_version":1,"profile_id":"fixture",
            "binding_id":"fixed-host","binding_version":"1"},
        "wire":{"profile":"fixture","format_version":"1","producer_build":"fixture"},
        "validator_id":"fixed-host","validator_version":"1",
        "observation_profile_digest":"11".repeat(32),
        "registration_digest":"22".repeat(32),"api":"fixture-api"
    })
}

#[test]
fn configure_uses_the_resolved_prompt_and_no_credential_fields() {
    let bytes = serde_json::to_vec(&installation()).unwrap();
    let selected = options(&bytes, "resolved private prompt").unwrap();
    let body: Value = serde_json::from_slice(&selected.configuration).unwrap();
    assert_eq!(
        body,
        json!({"version":1,"api":"fixture-api",
        "system_prompt":"resolved private prompt"})
    );
    assert_eq!(selected.executable, Path::new("/usr/bin/true"));
    assert!(Registration::new(selected).is_ok());
}

#[test]
fn malformed_registration_never_echoes_supplied_values() {
    let baseline = installation();
    let mut cases = Vec::new();
    for (key, value) in [
        ("version", json!(2)),
        ("api", json!("")),
        ("module", json!("private-canary")),
        ("registration_digest", json!("private-canary")),
        ("validator_id", json!("\nprivate-canary")),
    ] {
        let mut candidate = baseline.clone();
        candidate[key] = value;
        cases.push(serde_json::to_vec(&candidate).unwrap());
    }
    let valid = serde_json::to_string(&baseline).unwrap();
    cases.push(
        valid
            .replacen('{', "{\"api\":\"private-canary\",", 1)
            .into_bytes(),
    );
    for bytes in cases {
        let error = options(&bytes, "private-canary").err().unwrap();
        assert_eq!(error.code, "configuration");
        assert_eq!(
            error.message,
            "Native profile registration is unavailable or invalid"
        );
    }
    for prompt in [String::new(), "p".repeat(16_385)] {
        assert!(options(valid.as_bytes(), &prompt).is_err());
    }
}

#[test]
fn registration_file_requires_private_permissions_and_an_existing_fixed_host() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    std::fs::write(&path, serde_json::to_vec(&installation()).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(load(&path, "system").is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load(&path, "system").is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut candidate = installation();
    candidate["executable"] = json!("relative-host");
    std::fs::write(&path, serde_json::to_vec(&candidate).unwrap()).unwrap();
    assert!(load(&path, "system").is_err());
    candidate["executable"] = json!(root.path().join("absent-host"));
    std::fs::write(&path, serde_json::to_vec(&candidate).unwrap()).unwrap();
    assert!(load(&path, "system").is_err());
}
