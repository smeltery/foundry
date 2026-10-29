use foundry_contracts::{
    MAX_FRAME_BYTES,
    context::{ContextDigest, ContextId, ContextPayload, ContextReceipt, MAX_PAYLOAD_BYTES},
    context_observation::{self as wire, ObservationBinding, ObservationFailure, ObservationFrame},
    context_v2::LoadingScope,
};
use serde_json::{Value, json};

fn receipt(n: u8) -> ContextReceipt {
    ContextReceipt::new(format!("00000000-0000-4000-8000-{n:012}")).unwrap()
}
fn binding() -> ObservationBinding {
    ObservationBinding {
        scope: LoadingScope {
            restore_id: ContextId::new("restore").unwrap(),
            load_id: receipt(1),
        },
        job_id: receipt(2),
        begin_binding: ContextDigest::of(b"full descriptor"),
        profile_digest: ContextDigest::of(b"exact comparison profile"),
    }
}
fn observed(bytes: Vec<u8>) -> ObservationFrame {
    ObservationFrame::observed(
        binding(),
        receipt(3),
        ContextPayload::new(bytes).unwrap(),
        receipt(4),
    )
}
fn value() -> Value {
    serde_json::from_slice(&wire::encode(&observed(b"detached live body".to_vec())).unwrap())
        .unwrap()
}
fn reject(value: &Value) {
    assert!(wire::decode_response(&serde_json::to_vec(value).unwrap()).is_err());
}

#[test]
fn private_directional_roundtrip_and_receipt_last() {
    let request = ObservationFrame::request(binding(), receipt(4));
    let bytes = wire::encode(&request).unwrap();
    let request = wire::decode_request(&bytes).unwrap();
    assert!(request.binding() == &binding());
    assert!(request.challenge() == &receipt(4));
    assert!(request.payload().unwrap().is_none());
    assert!(request.check_response(&binding(), &receipt(4)).is_err());
    assert!(wire::decode_response(&bytes).is_err());

    let bytes = wire::encode(&observed(vec![0, 255, 4, 0])).unwrap();
    assert!(wire::decode_request(&bytes).is_err());
    assert!(
        String::from_utf8(bytes.clone())
            .unwrap()
            .ends_with(&format!("\"challenge\":\"{}\"}}", receipt(4).as_str()))
    );
    let response = wire::decode_response(&bytes).unwrap();
    response.check_response(&binding(), &receipt(4)).unwrap();
    assert!(response.instance_id() == Some(&receipt(3)));
    assert_eq!(
        response.payload().unwrap().unwrap().as_bytes(),
        [0, 255, 4, 0]
    );
    assert_eq!(response.failure(), None);
}

#[test]
fn every_correlation_field_and_challenge_are_required() {
    let response = observed(b"observation".to_vec());
    for field in 0..5 {
        let mut wrong = binding();
        match field {
            0 => wrong.scope.restore_id = ContextId::new("another restore").unwrap(),
            1 => wrong.scope.load_id = receipt(5),
            2 => wrong.job_id = receipt(5),
            3 => wrong.begin_binding = ContextDigest::of(b"different descriptor"),
            _ => wrong.profile_digest = ContextDigest::of(b"different profile"),
        }
        assert!(response.check_response(&wrong, &receipt(4)).is_err());
    }
    assert!(response.check_response(&binding(), &receipt(5)).is_err());
}

#[test]
fn rejection_is_correlated_but_never_contains_observation_or_instance() {
    for code in [
        ObservationFailure::Unsupported,
        ObservationFailure::InvalidState,
        ObservationFailure::Limit,
        ObservationFailure::Validation,
        ObservationFailure::Cancelled,
    ] {
        let bytes = wire::encode(&ObservationFrame::rejected(binding(), code, receipt(4))).unwrap();
        let response = wire::decode_response(&bytes).unwrap();
        response.check_response(&binding(), &receipt(4)).unwrap();
        assert_eq!(response.failure(), Some(code));
        assert!(response.payload().unwrap().is_none());
        assert!(response.instance_id().is_none());
        assert!(wire::decode_request(&bytes).is_err());
    }
}

#[test]
fn duplicate_unknown_and_missing_nested_fields_fail() {
    let original = value();
    for pointer in ["", "/binding", "/binding/scope"] {
        let mut extra = original.clone();
        extra
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(true));
        reject(&extra);
        let object = original.pointer(pointer).unwrap().as_object().unwrap();
        for field in object.keys() {
            let mut missing = original.clone();
            missing
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            reject(&missing);
            let text = serde_json::to_string(&original).unwrap();
            let key = format!("\"{field}\":");
            let duplicated = text.replacen(&key, &format!("{key}{},{key}", object[field]), 1);
            assert!(wire::decode_response(duplicated.as_bytes()).is_err());
        }
    }
}

#[test]
fn payload_integrity_encoding_identities_and_bounds_are_enforced() {
    for (pointer, replacement) in [
        ("/payload_length", json!(0)),
        ("/payload_length", json!(u64::MAX)),
        (
            "/payload_digest",
            json!(ContextDigest::of(b"other").to_hex()),
        ),
        ("/payload_digest", json!("A".repeat(64))),
        ("/payload_base64", json!("")),
        ("/payload_base64", json!("ZE==")),
        ("/payload_base64", json!("ZA")),
        ("/instance_id", json!("invalid")),
        ("/binding/job_id", json!("invalid")),
        ("/challenge", json!("invalid")),
        ("/type", json!("context_restored_v2")),
    ] {
        let mut changed = value();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        reject(&changed);
    }
    let exact = observed(vec![255; MAX_PAYLOAD_BYTES]);
    let bytes = wire::encode(&exact).unwrap();
    assert!(bytes.len() < MAX_FRAME_BYTES);
    assert_eq!(
        wire::decode_response(&bytes)
            .unwrap()
            .payload()
            .unwrap()
            .unwrap()
            .as_bytes()
            .len(),
        MAX_PAYLOAD_BYTES
    );
    assert!(ContextPayload::new(vec![0; MAX_PAYLOAD_BYTES + 1]).is_err());
    assert!(wire::decode_response(&vec![b' '; MAX_FRAME_BYTES + 1]).is_err());
    assert!(wire::decode_request(&vec![b' '; MAX_FRAME_BYTES + 1]).is_err());
}
