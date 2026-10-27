use foundry_contracts::{context::*, context_v2::*};
use serde_json::json;
fn id(text: &str) -> ContextId {
    ContextId::new(text).unwrap()
}
fn receipt() -> ContextReceipt {
    ContextReceipt::new("01234567-89ab-4cde-8f01-23456789abcd").unwrap()
}
fn scope() -> LoadingScope {
    LoadingScope {
        restore_id: id("restore"),
        load_id: receipt(),
    }
}
fn descriptor() -> RestoreDescriptor {
    RestoreDescriptor {
        capture_id: id("capture"),
        checkpoint_id: id("checkpoint"),
        record_digest: ContextDigest::of(b"record"),
        witness_digest: ContextDigest::of(b"witness"),
        format: ContextFormat {
            profile: id("profile"),
            format_version: id("1"),
            producer_build: id("fixture"),
        },
        configuration_digest: ContextDigest::of(b"config"),
        payload_length: 4,
        payload_digest: ContextDigest::of(b"body"),
        retention_binding: ContextDigest::of(b"retention"),
    }
}
fn continuation(text: &str) -> ContinuationIdentity {
    ContinuationIdentity {
        attempt_id: id("actual-attempt"),
        continuation_id: id("continue"),
        input_id: id("message"),
        input_sequence: 1,
        input_digest: ContextDigest::of(text.as_bytes()),
        input_bytes: text.len() as u64,
        authorization_binding: ContextDigest::of(b"grant"),
    }
}
#[test]
fn roundtrip_all_frames_tail_receipts_and_no_loading_attempt() {
    let binding = restore_binding(&scope(), &descriptor()).unwrap();
    let payload = ContextPayload::new(b"body".to_vec()).unwrap();
    let frames = [
        LoadingFrame::begin(scope(), descriptor()).unwrap(),
        LoadingFrame::prepared(scope(), binding),
        LoadingFrame::payload(scope(), binding, &payload, receipt()).unwrap(),
        LoadingFrame::restored(scope(), binding, payload.digest(), receipt()),
        LoadingFrame::continuation(scope(), continuation("new input"), "new input", receipt())
            .unwrap(),
        LoadingFrame::accepted(scope(), continuation("new input"), receipt()).unwrap(),
    ];
    for frame in frames {
        let bytes = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(decoded.kind(), frame.kind());
        decoded.check_scope(&scope()).unwrap();
        assert_eq!(encode_frame(&decoded).unwrap(), bytes);
        if frame.receipt().is_some() {
            assert!(
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .ends_with(&format!("\"receipt\":\"{}\"}}", receipt().as_str()))
            );
        }
        if !matches!(frame.kind(), FrameKind::Continue | FrameKind::Accepted) {
            assert!(!std::str::from_utf8(&bytes).unwrap().contains("attempt_id"));
        }
        assert!(foundry_contracts::context::decode_command(&bytes).is_err());
        assert!(foundry_contracts::context::decode_event(&bytes).is_err());
        assert!(serde_json::from_slice::<foundry_contracts::AdapterEvent>(&bytes).is_err());
    }
}
#[test]
fn nested_duplicates_unknown_fields_and_v1_never_decode() {
    let original = String::from_utf8(
        encode_frame(&LoadingFrame::begin(scope(), descriptor()).unwrap()).unwrap(),
    )
    .unwrap();
    for changed in [
        original.replace(
            "\"restore_id\":\"restore\"",
            "\"restore_id\":\"restore\",\"restore_id\":\"other\"",
        ),
        original.replace(
            "\"profile\":\"profile\"",
            "\"profile\":\"profile\",\"profile\":\"other\"",
        ),
        original.replace(
            "\"payload_length\":4",
            "\"payload_length\":4,\"payload_length\":4",
        ),
        original.replace("\"scope\":{", "\"scope\":{\"attempt_id\":\"fake\","),
        original.replace("restore_begin_v2", "restore_begin"),
        original.replace("\"type\":", "\"type\":\"restore_begin_v2\",\"type\":"),
    ] {
        assert!(
            decode_frame(changed.as_bytes()).is_err(),
            "accepted altered schema"
        );
    }
    let original = String::from_utf8(
        encode_frame(&LoadingFrame::accepted(scope(), continuation("x"), receipt()).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(
        decode_frame(
            original
                .replace(
                    "\"input_sequence\":1",
                    "\"input_sequence\":1,\"input_sequence\":1"
                )
                .as_bytes()
        )
        .is_err()
    );
    assert!(decode_frame(&vec![b' '; foundry_contracts::MAX_FRAME_BYTES + 1]).is_err());
}
#[test]
fn scope_and_descriptor_substitution_cannot_rebind_a_load() {
    let expected = scope();
    let binding = restore_binding(&expected, &descriptor()).unwrap();
    let mut other = scope();
    other.load_id = ContextReceipt::new("11234567-89ab-4cde-8f01-23456789abcd").unwrap();
    let foreign = LoadingFrame::prepared(other.clone(), binding);
    assert!(foreign.check_scope(&expected).is_err());
    assert!(restore_binding(&other, &descriptor()).unwrap() != binding);
    other = scope();
    other.restore_id = id("other");
    assert!(restore_binding(&other, &descriptor()).unwrap() != binding);
    let mut changed = descriptor();
    changed.retention_binding = ContextDigest::of(b"other");
    assert!(restore_binding(&expected, &changed).unwrap() != binding);
    LoadingFrame::prepared(scope(), binding)
        .check_scope(&expected)
        .unwrap();
    // Observing either frame never changes the caller's retained loading identity.
    assert!(expected == scope());
}
#[test]
fn payload_hash_length_canonical_encoding_and_exact_maximum() {
    let binding = ContextDigest::of(b"binding");
    let payload = ContextPayload::new(vec![7; MAX_PAYLOAD_BYTES]).unwrap();
    let frame = LoadingFrame::payload(scope(), binding, &payload, receipt()).unwrap();
    let bytes = encode_frame(&frame).unwrap();
    assert_eq!(
        decode_frame(&bytes)
            .unwrap()
            .decode_payload()
            .unwrap()
            .unwrap()
            .as_bytes(),
        payload.as_bytes()
    );
    let small = LoadingFrame::payload(
        scope(),
        binding,
        &ContextPayload::new(vec![0]).unwrap(),
        receipt(),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encode_frame(&small).unwrap()).unwrap();
    for (field, new) in [
        ("payload_length", json!(2)),
        ("payload_digest", json!("00".repeat(32))),
        ("payload_base64", json!("AB==")),
        ("payload_base64", json!("AA")),
        ("payload_length", json!(MAX_PAYLOAD_BYTES + 1)),
    ] {
        let mut changed = value.clone();
        changed[field] = new;
        assert!(decode_frame(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
}
#[test]
fn input_bounds_hashes_and_durable_sequence_limits() {
    let text = "é".repeat(foundry_contracts::MAX_PROMPT_BYTES / 2);
    assert!(LoadingFrame::continuation(scope(), continuation(&text), &text, receipt()).is_ok());
    for text in [
        " ".into(),
        "x".repeat(foundry_contracts::MAX_PROMPT_BYTES + 1),
    ] {
        assert!(
            LoadingFrame::continuation(scope(), continuation(&text), &text, receipt()).is_err()
        );
    }
    let mut wrong = continuation("text");
    wrong.input_digest = ContextDigest::of(b"other");
    assert!(LoadingFrame::continuation(scope(), wrong, "text", receipt()).is_err());
    for sequence in [0, u64::MAX] {
        let mut wrong = continuation("text");
        wrong.input_sequence = sequence;
        assert!(LoadingFrame::accepted(scope(), wrong, receipt()).is_err());
    }
    assert!(ContextId::new("é".repeat(64)).is_ok());
    assert!(ContextId::new("é".repeat(65)).is_err());
}

#[test]
fn receipt_and_complete_continuation_correlation_are_inert_exact_comparisons() {
    let binding = ContextDigest::of(b"binding");
    let payload = ContextDigest::of(b"payload");
    let prepared = LoadingFrame::prepared(scope(), binding);
    prepared.check_prepared(&scope(), binding).unwrap();
    assert!(prepared.check_prepared(&scope(), payload).is_err());
    let restored = LoadingFrame::restored(scope(), binding, payload, receipt());
    restored
        .check_restored(&scope(), binding, payload, &receipt())
        .unwrap();
    let other_receipt = ContextReceipt::new("11234567-89ab-4cde-8f01-23456789abcd").unwrap();
    assert!(
        restored
            .check_restored(&scope(), binding, payload, &other_receipt)
            .is_err()
    );
    let identity = continuation("input");
    let accepted = LoadingFrame::accepted(scope(), identity.clone(), receipt()).unwrap();
    accepted
        .check_accepted(&scope(), &identity, &receipt())
        .unwrap();
    for mode in 0..7 {
        let mut changed = identity.clone();
        match mode {
            0 => changed.attempt_id = id("other"),
            1 => changed.continuation_id = id("other"),
            2 => changed.input_id = id("other"),
            3 => changed.input_sequence += 1,
            4 => changed.input_digest = payload,
            5 => changed.input_bytes += 1,
            _ => changed.authorization_binding = payload,
        }
        assert!(
            accepted
                .check_accepted(&scope(), &changed, &receipt())
                .is_err()
        );
    }
    assert!(
        accepted
            .check_accepted(&scope(), &identity, &other_receipt)
            .is_err()
    );
    // Repeated comparison is not a consuming permit or process scope transition.
    accepted
        .check_accepted(&scope(), &identity, &receipt())
        .unwrap();
    assert!(
        prepared
            .check_accepted(&scope(), &identity, &receipt())
            .is_err()
    );
}
