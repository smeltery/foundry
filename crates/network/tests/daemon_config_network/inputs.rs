use super::support::*;
use foundry_network::daemon_config::ConfigError;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn ownership_check_uses_bound_resolved_trust_and_actual_endpoint() {
    let root = Root::new();
    root.write("keys.json", include_bytes!("../proof_support/jwks.json"));
    let mut value = network();
    value["trust"] = json!({"mode":"proof","issuer":"fixture-issuer","audience":"fixture-audience",
        "token_type":"local-proof","jwks_file":"keys.json"});
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../proof_support/claims.json")).unwrap();
    let endpoint = fixtures["endpoint"].as_str().unwrap().parse().unwrap();
    let token = |name: &str| {
        fixtures["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .unwrap()["token"]
            .as_str()
            .unwrap()
    };
    resolved
        .verify_ownership_proof(&endpoint, token("valid"), 1000)
        .unwrap();
    resolved
        .verify_ownership_proof(&endpoint, token("expired"), 1000)
        .unwrap();
    assert_eq!(
        resolved.verify_ownership_proof(&endpoint, token("forged"), 1000),
        Err(ConfigError::Invalid)
    );
    assert_eq!(
        resolved.verify_ownership_proof(&endpoint, token("future_excess"), 1000),
        Err(ConfigError::Invalid)
    );
    assert_eq!(
        resolved.verify_ownership_proof(
            &iroh::SecretKey::generate().public(),
            token("valid"),
            1000
        ),
        Err(ConfigError::Invalid)
    );
    root.write("keys.json", b"changed trust file");
    resolved
        .verify_ownership_proof(&endpoint, token("valid"), 1000)
        .unwrap();
    assert!(enabled(&config).resolve(&root.0).is_err());
    let unverified_config = root.load(network());
    let unverified = enabled(&unverified_config).resolve(&root.0).unwrap();
    assert_eq!(
        unverified.verify_ownership_proof(&endpoint, token("valid"), 1000),
        Err(ConfigError::Invalid)
    );
}

#[test]
fn pinned_codebook_uses_exact_bytes_and_resolved_inputs_do_not_reread_files() {
    let root = Root::new();
    let mut value = network();
    value["codebook"] = codebook(&root);
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let policy = resolved.code_policy().unwrap().unwrap();
    let code = policy.normalize("BIRCH / cobalt, dahlia ELM").unwrap();
    assert_eq!(code.expose(), "birch-cobalt-dahlia-elm");
    let binding = policy.binding();
    root.write("dictionary.txt", b"different\n");
    assert_eq!(resolved.code_policy().unwrap().unwrap().binding(), binding);
    resolved.store_policy().unwrap();
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
}

#[test]
fn referenced_public_inputs_accept_explicit_absolute_parent_alias_and_shared_paths() {
    let root = Root::new();
    let mut value = network();
    let mut book = codebook(&root);
    fs::create_dir(root.0.join("base")).unwrap();
    symlink(root.0.join("dictionary.txt"), root.0.join("alias.txt")).unwrap();
    fs::hard_link(root.0.join("dictionary.txt"), root.0.join("shared.txt")).unwrap();
    fs::set_permissions(
        root.0.join("dictionary.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    for path in [
        root.0.join("dictionary.txt"),
        root.0.join("alias.txt"),
        root.0.join("shared.txt"),
        "../dictionary.txt".into(),
    ] {
        book["path"] = json!(path);
        value["codebook"] = book.clone();
        let config = root.load(value.clone());
        assert!(
            enabled(&config)
                .resolve(&root.0.join("base"))
                .unwrap()
                .code_policy()
                .unwrap()
                .is_some()
        );
    }
    let config = root.load(value);
    assert!(matches!(
        enabled(&config).resolve(std::path::Path::new("relative")),
        Err(ConfigError::Invalid)
    ));
}

#[test]
fn every_explicit_der_root_is_validated_without_falling_back_to_other_roots() {
    let root = Root::new();
    let (certs, _) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    root.write("relay.der", certs[0].as_ref());
    let mut value = network();
    value["transport"]["direct_paths"] = json!(false);
    value["transport"]["relays"] = json!(["https://127.0.0.1:4567"]);
    value["transport"]["relay_root_certificates"] = json!(["relay.der"]);
    let config = root.load(value.clone());
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    let roots = resolved
        .endpoint_config(iroh::SecretKey::generate())
        .relay_root_certificates;
    assert_eq!(roots, vec![certs[0].to_vec()]);
    root.write("invalid.der", b"not a certificate");
    value["transport"]["relay_root_certificates"] = json!(["relay.der", "invalid.der"]);
    let config = root.load(value);
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
}

#[test]
fn trust_uses_actual_public_jwks_and_rejects_corruption_or_private_key_material() {
    let root = Root::new();
    let original = include_bytes!("../proof_support/jwks.json");
    root.write("keys.json", original);
    let mut value = network();
    value["trust"] = json!({"mode":"proof","issuer":"fixture-issuer","audience":"fixture-audience",
        "token_type":"local-proof","jwks_file":"keys.json"});
    let config = root.load(value);
    let resolved = enabled(&config).resolve(&root.0).unwrap();
    resolved.store_policy().unwrap();
    root.write("keys.json", b"malformed-private-marker");
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
    resolved.store_policy().unwrap();
    let mut keys: serde_json::Value = serde_json::from_slice(original).unwrap();
    keys["keys"][0]["d"] = json!("private-marker");
    root.write("keys.json", &serde_json::to_vec(&keys).unwrap());
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
}

#[test]
fn references_are_bounded_regular_file_reads_and_loading_does_not_resolve_them() {
    use std::os::unix::ffi::OsStrExt;
    let root = Root::new();
    let mut value = network();
    value["codebook"] = codebook(&root);
    root.write("dictionary.txt", &vec![b'a'; 262145]);
    let config = root.load(value.clone()); // Structural validation performs no referenced IO.
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
    fs::remove_file(root.0.join("dictionary.txt")).unwrap();
    let config = root.load(value.clone());
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Storage)
    ));
    fs::create_dir(root.0.join("dictionary.txt")).unwrap();
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::UnsafeFile)
    ));
    fs::remove_dir(root.0.join("dictionary.txt")).unwrap();
    let path =
        std::ffi::CString::new(root.0.join("dictionary.txt").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::UnsafeFile)
    ));
    value["transport"]["relay_root_certificates"] = json!(["large.der"]);
    value["codebook"] = serde_json::Value::Null;
    root.write("large.der", &vec![b'a'; 65537]);
    let config = root.load(value);
    assert!(matches!(
        enabled(&config).resolve(&root.0),
        Err(ConfigError::Invalid)
    ));
}
