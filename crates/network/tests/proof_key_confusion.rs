//! HMAC tokens keyed with the trusted RSA public key must never verify.
use data_encoding::{BASE64, BASE64URL_NOPAD};
use foundry_network::proof::{ProofConfig, ProofError, ProofUse, ProofVerifier};
use iroh::EndpointId;
use ring::{hmac, signature};
use serde_json::Value;

fn verifier() -> ProofVerifier {
    let config = ProofConfig {
        issuer: "fixture-issuer".into(),
        audience: "fixture-audience".into(),
        token_type: "local-proof".into(),
    };
    ProofVerifier::from_jwks(config, include_bytes!("proof_support/jwks.json")).unwrap()
}
fn vector(file: &str, name: &str) -> String {
    let document: Value = serde_json::from_str(file).unwrap();
    document["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}
fn endpoint() -> EndpointId {
    let claims: Value = serde_json::from_str(include_str!("proof_support/claims.json")).unwrap();
    claims["endpoint"].as_str().unwrap().parse().unwrap()
}
fn jwk_integer(field: &str) -> Vec<u8> {
    let jwks: Value = serde_json::from_slice(include_bytes!("proof_support/jwks.json")).unwrap();
    BASE64URL_NOPAD
        .decode(jwks["keys"][0][field].as_str().unwrap().as_bytes())
        .unwrap()
}
fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 128 {
        out.push(body.len() as u8);
    } else {
        let length = body.len().to_be_bytes();
        let skip = length.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (length.len() - skip) as u8);
        out.extend_from_slice(&length[skip..]);
    }
    out.extend_from_slice(body);
    out
}
fn der_integer(value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    if value[0] & 0x80 != 0 {
        body.push(0);
    }
    body.extend_from_slice(value);
    der(0x02, &body)
}
/// PKCS#1 RSAPublicKey for the fixture's modulus and exponent.
fn pkcs1() -> Vec<u8> {
    der(
        0x30,
        &[
            der_integer(&jwk_integer("n")),
            der_integer(&jwk_integer("e")),
        ]
        .concat(),
    )
}
/// SubjectPublicKeyInfo wrapping the PKCS#1 key with the rsaEncryption OID.
fn spki() -> Vec<u8> {
    let algorithm = der(
        0x30,
        &[
            &[
                0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
            ][..],
            &[0x05, 0x00],
        ]
        .concat(),
    );
    let mut bits = vec![0];
    bits.extend(pkcs1());
    der(0x30, &[algorithm, der(0x03, &bits)].concat())
}
fn pem() -> String {
    let encoded = BASE64.encode(&spki());
    let mut text = String::from("-----BEGIN PUBLIC KEY-----\n");
    for line in encoded.as_bytes().chunks(64) {
        text.push_str(std::str::from_utf8(line).unwrap());
        text.push('\n');
    }
    text.push_str("-----END PUBLIC KEY-----\n");
    text
}
/// Every byte form a confused verifier might feed to HMAC as the shared secret.
fn public_key_forms() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("pem", pem().into_bytes()),
        ("spki", spki()),
        ("pkcs1", pkcs1()),
        ("modulus", jwk_integer("n")),
        ("jwks", include_bytes!("proof_support/jwks.json").to_vec()),
    ]
}
fn signing_input(token: &str) -> &str {
    &token[..token.rfind('.').unwrap()]
}
fn mac(key: &[u8], input: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    BASE64URL_NOPAD.encode(hmac::sign(&key, input.as_bytes()).as_ref())
}
fn header(alg: &str) -> String {
    BASE64URL_NOPAD
        .encode(format!(r#"{{"alg":"{alg}","kid":"fixture-a","typ":"local-proof"}}"#).as_bytes())
}

#[test]
fn encoded_public_key_matches_the_trusted_jwk() {
    // Guard the key forms below: ring accepts this DER only if it is the real key.
    let valid = vector(include_str!("proof_support/claims.json"), "valid");
    let (input, signature_text) = valid.rsplit_once('.').unwrap();
    signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, pkcs1())
        .verify(
            input.as_bytes(),
            &BASE64URL_NOPAD.decode(signature_text.as_bytes()).unwrap(),
        )
        .unwrap();
    assert!(pem().starts_with("-----BEGIN PUBLIC KEY-----\nMIIBIjAN"));
}

#[test]
fn stored_hs256_vector_is_an_hmac_under_the_public_key_and_is_refused() {
    let token = vector(include_str!("proof_support/headers.json"), "hs256");
    let input = signing_input(&token);
    assert_eq!(&token[input.len() + 1..], mac(pem().as_bytes(), input));
    assert_eq!(
        verifier()
            .verify(&endpoint(), &token, ProofUse::Ownership, 1000)
            .unwrap_err(),
        ProofError::Algorithm
    );
}

#[test]
fn hmac_under_any_public_key_form_never_verifies() {
    let verifier = verifier();
    let valid = vector(include_str!("proof_support/claims.json"), "valid");
    let payload = valid.split('.').nth(1).unwrap();
    for (form, key) in public_key_forms() {
        for usage in [ProofUse::Current, ProofUse::Ownership] {
            let input = format!("{}.{payload}", header("HS256"));
            let token = format!("{input}.{}", mac(&key, &input));
            assert_eq!(
                verifier
                    .verify(&endpoint(), &token, usage, 1000)
                    .unwrap_err(),
                ProofError::Algorithm,
                "{form}"
            );
            // Relabelling the HMAC as RS256 reaches RSA verification and fails there.
            let input = format!("{}.{payload}", header("RS256"));
            let token = format!("{input}.{}", mac(&key, &input));
            assert_eq!(
                verifier
                    .verify(&endpoint(), &token, usage, 1000)
                    .unwrap_err(),
                ProofError::Signature,
                "{form}"
            );
        }
    }
}
