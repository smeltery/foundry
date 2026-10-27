use super::*;

#[test]
fn fixed_derivation_vectors_match_independent_implementations() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/code-derivation.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let value = |key: &str| fixture[key].as_str().unwrap();
        let mut policy = policy();
        let digest = data_encoding::HEXLOWER
            .decode(value("dictionary_sha256").as_bytes())
            .unwrap();
        policy.book = Codebook::from_bytes(
            value("dictionary_utf8").as_bytes(),
            digest.try_into().unwrap(),
        )
        .unwrap();
        policy.endpoint_context = value("endpoint_context").into();
        policy.secret_context = value("secret_context").into();
        let code = policy.normalize(value("canonical_code_utf8")).unwrap();
        let derived = policy.derive(&code).unwrap();
        assert_eq!(
            data_encoding::HEXLOWER.encode(&derived.endpoint_key().to_bytes()),
            value("endpoint_seed_hex")
        );
        assert_eq!(
            data_encoding::HEXLOWER.encode(derived.endpoint_key().public().as_bytes()),
            value("endpoint_public_key_hex")
        );
        assert_eq!(derived.bearer_secret(), value("bearer_utf8"));
        assert_eq!(
            data_encoding::HEXLOWER.encode(&derived.invitation_key()),
            value("invitation_map_key_hex")
        );
    }
}

fn policy() -> CodePolicy {
    let bytes = b"amber\nbirch\ncedar\ndune\n";
    let digest: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    CodePolicy::new(
        Codebook::from_bytes(bytes, digest).unwrap(),
        &WireProfile {
            team_alpn: b"team".to_vec(),
            join_alpn: b"join".to_vec(),
            protocol: 1,
            signing_prefix: "test-signing".into(),
            invitation_endpoint_context: "test-endpoint".into(),
            invitation_secret_context: "test-secret".into(),
            ticket_prefixes: vec!["test".into()],
        },
    )
    .unwrap()
}

#[test]
fn normalization_uses_ascii_separators_and_exact_four_words() {
    let policy = policy();
    assert_eq!(
        policy
            .normalize("AMBER1birch 雪 cedar--AMBER")
            .unwrap()
            .expose(),
        "amber-birch-cedar-amber"
    );
    for input in [
        "amber-birch-cedar",
        "amber-birch-cedar-amber-birch",
        "amber-birch-cedar-other",
        "ámbér birch cedar amber",
    ] {
        assert!(policy.normalize(input).is_err());
    }
    assert!(policy.normalize(&"a".repeat(4097)).is_err());
}

#[test]
fn sampling_rejects_cutoff_uses_little_endian_and_allows_repeats() {
    let policy = policy();
    let mut values = [u32::MAX - 3, 0, 1, 2, 0].into_iter();
    let code = policy
        .generate_with(|bytes| {
            *bytes = values.next().unwrap().to_le_bytes();
            Ok(())
        })
        .unwrap();
    assert_eq!(code.expose(), "amber-birch-cedar-amber");
    assert!(values.next().is_none());
    assert!(
        policy
            .generate_with(|_| Err(NetworkError::Invalid("entropy failed")))
            .is_err()
    );
}

#[test]
fn derivation_uses_distinct_contexts_and_hashes_hex_bearer() {
    let policy = policy();
    let code = policy.normalize("amber birch cedar amber").unwrap();
    let result = policy.derive(&code).unwrap();
    let seed = blake3::derive_key("test-endpoint", b"amber-birch-cedar-amber");
    let secret = blake3::derive_key("test-secret", b"amber-birch-cedar-amber");
    assert_eq!(
        result.endpoint_key().public(),
        iroh::SecretKey::from_bytes(&seed).public()
    );
    assert_eq!(
        result.bearer_secret(),
        data_encoding::HEXLOWER.encode(&secret[..16])
    );
    assert_eq!(
        result.invitation_key(),
        *blake3::hash(result.bearer_secret().as_bytes()).as_bytes()
    );
    assert_ne!(
        result.invitation_key(),
        *blake3::hash(&secret[..16]).as_bytes()
    );
}

#[test]
fn dictionaries_require_exact_digest_unique_lowercase_entries_and_limits() {
    for bytes in [
        b"".as_slice(),
        b"amber\namber\n",
        b"Amber\n",
        b"amber\n\n",
        b"amber birch\n",
    ] {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        assert!(Codebook::from_bytes(bytes, digest).is_err());
    }
    assert!(Codebook::from_bytes(b"amber\n", [0; 32]).is_err());
    assert!(Codebook::from_bytes(&vec![b'a'; 262_145], [0; 32]).is_err());
}

#[test]
fn generated_codes_fit_normalization_at_word_limit_and_revalidate_membership() {
    let make_book = |bytes: &[u8]| {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        Codebook::from_bytes(bytes, digest)
    };
    let mut long = policy();
    long.book = make_book(&vec![b'a'; 1023]).unwrap();
    let generated = long
        .generate_with(|bytes| {
            *bytes = [0; 4];
            Ok(())
        })
        .unwrap();
    assert_eq!(generated.expose().len(), 4095);
    assert!(long.normalize(&format!(" {}", generated.expose())).is_ok());
    assert!(
        long.normalize(&format!("  {}", generated.expose()))
            .is_err()
    );
    assert_eq!(
        long.normalize(generated.expose()).unwrap().expose(),
        generated.expose()
    );
    assert!(long.derive(&generated).is_ok());
    assert!(policy().derive(&generated).is_err());
    assert!(make_book(&vec![b'a'; 1024]).is_err());
    assert!(make_book(&vec![b'a'; 262_145]).is_err());
}

#[test]
fn non_power_of_two_sampling_and_dictionary_order_are_preserved() {
    let book = |bytes: &[u8]| {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        Codebook::from_bytes(bytes, digest).unwrap()
    };
    let mut selected = policy();
    selected.book = book(b"amber\nbirch\ncedar\n");
    let mut samples = [u32::MAX, u32::MAX - 1, 0, 1, 2].into_iter();
    let code = selected
        .generate_with(|bytes| {
            *bytes = samples.next().unwrap().to_le_bytes();
            Ok(())
        })
        .unwrap();
    assert_eq!(code.expose(), "cedar-amber-birch-cedar");
    assert!(samples.next().is_none());
    let original = selected.derive(&code).unwrap();
    selected.book = book(b"cedar\nbirch\namber\n");
    assert_eq!(
        selected.derive(&code).unwrap().invitation_key(),
        original.invitation_key()
    );
    assert_eq!(
        selected
            .generate_with(|bytes| {
                *bytes = [0; 4];
                Ok(())
            })
            .unwrap()
            .expose(),
        "cedar-cedar-cedar-cedar"
    );
}

#[test]
fn dictionary_entry_count_limit_is_checked_with_valid_digests() {
    let words: Vec<String> = (0..4097)
        .map(|i| {
            [i / 676, i / 26 % 26, i % 26]
                .into_iter()
                .map(|n| char::from(b'a' + n as u8))
                .collect()
        })
        .collect();
    for (count, accepted) in [(4096, true), (4097, false)] {
        let text = words[..count].join("\n");
        let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
            .as_ref()
            .try_into()
            .unwrap();
        assert_eq!(
            Codebook::from_bytes(text.as_bytes(), digest).is_ok(),
            accepted
        );
    }
}
