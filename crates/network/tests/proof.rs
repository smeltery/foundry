use foundry_network::proof::{ProofConfig, ProofError, ProofUse, ProofVerifier};
use iroh::EndpointId;
use serde_json::{Value, json};

fn config() -> ProofConfig {
    ProofConfig {
        issuer: "fixture-issuer".into(),
        audience: "fixture-audience".into(),
        token_type: "local-proof".into(),
    }
}
fn verifier() -> ProofVerifier {
    ProofVerifier::from_jwks(config(), include_bytes!("proof_support/jwks.json")).unwrap()
}
fn fixtures() -> Value {
    serde_json::from_str(include_str!("proof_support/claims.json")).unwrap()
}
fn endpoint() -> EndpointId {
    fixtures()["endpoint"].as_str().unwrap().parse().unwrap()
}
fn token(name: &str) -> String {
    fixtures()["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn real_signatures_validate_claims_and_closed_headers() {
    let verifier = verifier();
    let headers: Value = serde_json::from_str(include_str!("proof_support/headers.json")).unwrap();
    for document in [fixtures(), headers] {
        for row in document["vectors"].as_array().unwrap() {
            let result = verifier.verify(
                &endpoint(),
                row["token"].as_str().unwrap(),
                ProofUse::Current,
                1000,
            );
            if let Some(error) = row["error"].as_str() {
                assert_eq!(
                    format!("{:?}", result.unwrap_err()),
                    error,
                    "{}",
                    row["name"]
                );
            } else {
                assert!(result.is_ok(), "{}: {result:?}", row["name"]);
            }
        }
    }
}

#[test]
fn ownership_skips_only_expiry_and_keeps_future_and_endpoint_checks() {
    let verifier = verifier();
    for name in ["expired", "expiry_boundary", "negative_times"] {
        assert_eq!(
            verifier
                .verify(&endpoint(), &token(name), ProofUse::Current, 1000)
                .unwrap_err(),
            ProofError::Expired
        );
        assert!(
            verifier
                .verify(&endpoint(), &token(name), ProofUse::Ownership, 1000)
                .is_ok()
        );
    }
    assert_eq!(
        verifier
            .verify(
                &endpoint(),
                &token("future_excess"),
                ProofUse::Ownership,
                1000
            )
            .unwrap_err(),
        ProofError::Future
    );
    assert!(
        verifier
            .verify(
                &endpoint(),
                &token("clock_max"),
                ProofUse::Ownership,
                i64::MAX
            )
            .is_ok()
    );
    assert_eq!(
        verifier
            .verify(
                &endpoint(),
                &token("clock_max"),
                ProofUse::Current,
                i64::MAX
            )
            .unwrap_err(),
        ProofError::Expired
    );
    for usage in [ProofUse::Current, ProofUse::Ownership] {
        let other = iroh::SecretKey::from_bytes(&[9; 32]).public();
        assert_eq!(
            verifier
                .verify(&other, &token("valid"), usage, 1000)
                .unwrap_err(),
            ProofError::Endpoint
        );
        assert_eq!(
            verifier
                .verify(&endpoint(), &token("forged"), usage, 1000)
                .unwrap_err(),
            ProofError::Signature
        );
    }
}

#[test]
fn expiry_boundaries_and_reversed_times_match_profile() {
    let verifier = verifier();
    assert!(
        verifier
            .verify(&endpoint(), &token("valid"), ProofUse::Current, 1999)
            .is_ok()
    );
    assert_eq!(
        verifier
            .verify(&endpoint(), &token("valid"), ProofUse::Current, 2000)
            .unwrap_err(),
        ProofError::Expired
    );
    assert!(
        verifier
            .verify(
                &endpoint(),
                &token("reversed_times"),
                ProofUse::Current,
                1000
            )
            .is_ok()
    );
}

#[test]
fn result_is_bound_to_input_and_redacts_sensitive_details() {
    let verifier = verifier();
    let original = token("valid");
    let padded = format!(" \n{original}\t ");
    let proof = verifier
        .verify(&endpoint(), &padded, ProofUse::Current, 1000)
        .unwrap();
    assert_eq!(proof.token_for_storage(), original);
    assert_eq!(proof.endpoint(), endpoint());
    assert_eq!(proof.subject(), "account-a");
    assert_eq!(proof.person(), "alex");
    assert_eq!(proof.email(), "alex@example.invalid");
    assert_eq!(proof.issued_at(), 1000);
    assert_eq!(proof.expires_at(), 2000);
    assert_eq!(proof.checked_at(), 1000);
    assert_eq!(proof.usage(), ProofUse::Current);
    assert_eq!(format!("{proof:?}"), "VerifiedProof { .. }");
    assert!(!format!("{verifier:?}").contains("fixture-issuer"));
    let mut other_config = config();
    other_config.issuer = "other".into();
    let other =
        ProofVerifier::from_jwks(other_config, include_bytes!("proof_support/jwks.json")).unwrap();
    assert_eq!(
        other
            .verify(&endpoint(), &original, ProofUse::Current, 1000)
            .unwrap_err(),
        ProofError::Issuer
    );
}

#[test]
fn replacement_order_uses_issued_time_then_trimmed_token_not_expiry() {
    let verifier = verifier();
    let old = verifier
        .verify(&endpoint(), &token("valid"), ProofUse::Ownership, 1000)
        .unwrap();
    let newer = verifier
        .verify(
            &endpoint(),
            &token("newer_shorter"),
            ProofUse::Ownership,
            1000,
        )
        .unwrap();
    assert!(newer.expires_at() < old.expires_at());
    assert!(newer.replacement_key() > old.replacement_key());
    let tie = verifier
        .verify(
            &endpoint(),
            &token("tie_changed"),
            ProofUse::Ownership,
            1000,
        )
        .unwrap();
    assert_eq!(
        tie.replacement_key().cmp(&old.replacement_key()),
        tie.token_for_storage().cmp(old.token_for_storage())
    );
    let repeat = verifier
        .verify(
            &endpoint(),
            &format!(" {} ", token("valid")),
            ProofUse::Ownership,
            1000,
        )
        .unwrap();
    assert_eq!(repeat.replacement_key(), old.replacement_key());
}

#[test]
fn exact_bytes_and_compact_encoding_are_authenticated() {
    use data_encoding::BASE64URL_NOPAD;
    let verifier = verifier();
    let original = token("valid");
    let parts: Vec<_> = original.split('.').collect();
    let mut claims: Value =
        serde_json::from_slice(&BASE64URL_NOPAD.decode(parts[1].as_bytes()).unwrap()).unwrap();
    claims["sub"] = json!("forged-account");
    let changed = BASE64URL_NOPAD.encode(&serde_json::to_vec(&claims).unwrap());
    assert_eq!(
        verifier
            .verify(
                &endpoint(),
                &format!("{}.{changed}.{}", parts[0], parts[2]),
                ProofUse::Current,
                1000
            )
            .unwrap_err(),
        ProofError::Signature
    );
    for malformed in [
        String::new(),
        "a.b".into(),
        "a.b.c.d".into(),
        format!("{}=.{}.{}", parts[0], parts[1], parts[2]),
        format!("{}.{}.{}=", parts[0], parts[1], parts[2]),
    ] {
        assert_eq!(
            verifier
                .verify(&endpoint(), &malformed, ProofUse::Current, 1000)
                .unwrap_err(),
            ProofError::Malformed
        );
    }
    for huge in ["A".repeat(4097), format!("{}{original}", " ".repeat(4096))] {
        assert_eq!(
            verifier
                .verify(&endpoint(), &huge, ProofUse::Ownership, 1000)
                .unwrap_err(),
            ProofError::TooLong
        );
    }
}

#[test]
fn explicit_key_rollover_retains_old_proofs_without_trying_unknown_keys() {
    let mut jwks: Value = serde_json::from_str(include_str!("proof_support/jwks.json")).unwrap();
    let mut second = jwks["keys"][0].clone();
    second["kid"] = json!("fixture-b");
    jwks["keys"].as_array_mut().unwrap().push(second);
    let verifier = ProofVerifier::from_jwks(config(), &serde_json::to_vec(&jwks).unwrap()).unwrap();
    let headers: Value = serde_json::from_str(include_str!("proof_support/headers.json")).unwrap();
    let second_token = headers["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "second_key")
        .unwrap()["token"]
        .as_str()
        .unwrap();
    for input in [token("valid"), second_token.into()] {
        assert!(
            verifier
                .verify(&endpoint(), &input, ProofUse::Ownership, 5000)
                .is_ok()
        );
    }
}
