use data_encoding::BASE64URL_NOPAD;
use foundry_network::proof::{ProofConfig, ProofError, ProofUse, ProofVerifier};
use serde_json::{Value, json};

fn config() -> ProofConfig {
    ProofConfig {
        issuer: "fixture-issuer".into(),
        audience: "fixture-audience".into(),
        token_type: "local-proof".into(),
    }
}
fn jwks() -> Value {
    serde_json::from_slice(include_bytes!("proof_support/jwks.json")).unwrap()
}
fn build(value: &Value) -> Result<ProofVerifier, ProofError> {
    ProofVerifier::from_jwks(config(), &serde_json::to_vec(value).unwrap())
}
fn verify_fixture(verifier: &ProofVerifier) -> Result<(), ProofError> {
    let vectors: Value = serde_json::from_str(include_str!("proof_support/claims.json")).unwrap();
    let endpoint = vectors["endpoint"].as_str().unwrap().parse().unwrap();
    verifier
        .verify(
            &endpoint,
            vectors["vectors"][0]["token"].as_str().unwrap(),
            ProofUse::Current,
            1000,
        )
        .map(|_| ())
}

#[test]
fn config_is_explicit_bounded_and_has_no_empty_trust_fallback() {
    for index in 0..3 {
        for value in [String::new(), "x".repeat(2049)] {
            let mut policy = config();
            match index {
                0 => policy.issuer = value,
                1 => policy.audience = value,
                _ => policy.token_type = value,
            }
            assert_eq!(
                ProofVerifier::from_jwks(policy, include_bytes!("proof_support/jwks.json"))
                    .unwrap_err(),
                ProofError::InvalidConfig
            );
        }
    }
    for bytes in [Vec::new(), b"{}".to_vec(), vec![b' '; 256 * 1024 + 1]] {
        assert_eq!(
            ProofVerifier::from_jwks(config(), &bytes).unwrap_err(),
            ProofError::InvalidConfig
        );
    }
    for keys in [Vec::new(), vec![jwks()["keys"][0].clone(); 65]] {
        assert_eq!(
            build(&json!({"keys":keys})).unwrap_err(),
            ProofError::InvalidConfig
        );
    }
}

#[test]
fn ambiguous_or_conflicting_keys_fail_policy_construction() {
    for (field, value) in [
        ("kty", json!("EC")),
        ("kid", json!("")),
        ("kid", json!("x".repeat(257))),
        ("alg", json!("HS256")),
        ("alg", Value::Null),
        ("use", json!("enc")),
        ("use", Value::Null),
        ("key_ops", json!([])),
        ("key_ops", Value::Null),
        ("key_ops", json!(["verify", "verify"])),
        ("key_ops", json!(["verify", "sign"])),
        ("n", json!("!")),
        ("e", json!("")),
        ("e", json!("AA")),
    ] {
        let mut document = jwks();
        document["keys"][0][field] = value;
        assert_eq!(
            build(&document).unwrap_err(),
            ProofError::InvalidConfig,
            "{field}"
        );
    }
    for field in ["d", "p", "q", "dp", "dq", "qi", "oth"] {
        for value in [json!("AA"), Value::Null] {
            let mut document = jwks();
            document["keys"][0][field] = value;
            assert_eq!(build(&document).unwrap_err(), ProofError::InvalidConfig);
        }
    }
    let document = json!({"keys":[jwks()["keys"][0],jwks()["keys"][0]]});
    assert_eq!(build(&document).unwrap_err(), ProofError::InvalidConfig);
}

#[test]
fn duplicate_security_members_are_not_silently_overwritten() {
    let original = serde_json::to_string(&jwks()["keys"][0]).unwrap();
    for duplicate in ["\"kid\":\"fixture-a\"", "\"alg\":\"RS256\"", "\"n\":\"AA\""] {
        let key = format!("{},{} }}", &original[..original.len() - 1], duplicate);
        let document = format!("{{\"keys\":[{key}]}}");
        assert_eq!(
            ProofVerifier::from_jwks(config(), document.as_bytes()).unwrap_err(),
            ProofError::InvalidConfig
        );
    }
    let document = format!("{{\"keys\":[{original}],\"keys\":[{original}]}}");
    assert_eq!(
        ProofVerifier::from_jwks(config(), document.as_bytes()).unwrap_err(),
        ProofError::InvalidConfig
    );
}

#[test]
fn rsa_bounds_and_ring_validation_fail_closed() {
    for len in [0, 128, 257 + 1024] {
        let mut document = jwks();
        document["keys"][0]["n"] = json!(BASE64URL_NOPAD.encode(&vec![255; len]));
        assert_eq!(build(&document).unwrap_err(), ProofError::InvalidConfig);
    }
    let mut document = jwks();
    document["keys"][0]["e"] = json!("Ag");
    assert_eq!(
        verify_fixture(&build(&document).unwrap()).unwrap_err(),
        ProofError::Signature
    );
}

#[test]
fn leading_zero_and_padded_public_integers_preserve_the_key() {
    let mut document = jwks();
    for field in ["n", "e"] {
        let mut bytes = vec![0, 0];
        bytes.extend(
            BASE64URL_NOPAD
                .decode(document["keys"][0][field].as_str().unwrap().as_bytes())
                .unwrap(),
        );
        document["keys"][0][field] = json!(format!("{}==", BASE64URL_NOPAD.encode(&bytes)));
    }
    assert!(verify_fixture(&build(&document).unwrap()).is_ok());
}

#[test]
fn optional_public_metadata_does_not_choose_keys_or_enable_fetching() {
    let mut document = jwks();
    let key = document["keys"][0].as_object_mut().unwrap();
    for field in ["alg", "use", "key_ops"] {
        key.remove(field);
    }
    key.insert("x5u".into(), json!("https://invalid.invalid/never-fetch"));
    key.insert("x5c".into(), json!(["ignored-public-metadata"]));
    assert!(verify_fixture(&build(&document).unwrap()).is_ok());
}
