use foundry_contracts::context::*;
use serde_json::{Value, json};

const RECEIPT: &str = "01234567-89ab-4cde-8f01-23456789abcd";
fn id(value: &str) -> ContextId {
    ContextId::new(value).unwrap()
}
fn receipt() -> ContextReceipt {
    ContextReceipt::new(RECEIPT).unwrap()
}
fn format() -> ContextFormat {
    ContextFormat {
        profile: id("fixture"),
        format_version: id("1"),
        producer_build: id("owned"),
    }
}
fn coverage() -> ContextCoverage {
    ContextCoverage {
        completed_turn_id: id("turn"),
        inputs: vec![InputCoverage {
            input_id: id("input"),
            input_digest: ContextDigest::of(b"input"),
            native_ids: vec![id("record")],
        }],
        results: vec![],
    }
}
fn candidate(payload: Vec<u8>) -> ContextEvent {
    ContextEvent::Candidate {
        attempt_id: id("attempt"),
        capture_id: id("capture"),
        challenge: receipt(),
        format: format(),
        coverage: coverage(),
        payload: ContextPayload::new(payload).unwrap(),
    }
}
fn candidate_value() -> Value {
    serde_json::from_slice(&encode_event(&candidate(b"private-sentinel".to_vec())).unwrap())
        .unwrap()
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn opaque_payload_roundtrips_at_exact_maximum_without_utf8_rewriting() {
    let payload: Vec<_> = (0..MAX_PAYLOAD_BYTES).map(|i| (i % 256) as u8).collect();
    let encoded = encode_event(&candidate(payload.clone())).unwrap();
    assert!(encoded.len() < foundry_contracts::MAX_FRAME_BYTES);
    assert!(is_context_frame(&encoded).unwrap());
    let ContextEvent::Candidate {
        payload: decoded, ..
    } = decode_event(&encoded).unwrap()
    else {
        panic!("candidate")
    };
    assert!(decoded.as_bytes() == payload);
    assert!(decoded.digest() == ContextDigest::of(&payload));
    assert!(ContextPayload::new(vec![]).is_err());
    assert!(ContextPayload::new(vec![0; MAX_PAYLOAD_BYTES + 1]).is_err());
}

#[test]
fn command_roundtrip_preserves_tail_receipts_and_literal_input() {
    let text = "literal \\\" $(false) Ω";
    let commands = [
        ContextCommand::Export {
            attempt_id: id("attempt"),
            capture_id: id("capture"),
            format_digest: ContextDigest::of(b"format"),
            cut_digest: ContextDigest::of(b"cut"),
            challenge: receipt(),
        },
        ContextCommand::Release {
            attempt_id: id("attempt"),
            operation_id: id("op"),
        },
        ContextCommand::RestoreBegin {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            checkpoint_id: id("saved"),
            record_digest: ContextDigest::of(b"record"),
            format: format(),
            configuration_digest: ContextDigest::of(b"config"),
            payload_length: 1,
            payload_digest: ContextDigest::of(b"a"),
        },
        ContextCommand::RestorePayload {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            payload: ContextPayload::new(vec![255, 0, 1]).unwrap(),
            receipt: receipt(),
        },
        ContextCommand::Continue {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            continuation_id: id("continue"),
            input_id: id("input"),
            input_digest: ContextDigest::of(text.as_bytes()),
            text: text.into(),
            receipt: receipt(),
        },
        ContextCommand::Failed {
            attempt_id: id("attempt"),
            operation_id: id("op"),
            code: ContextFailure::Cancelled,
        },
    ];
    for command in commands {
        let encoded = encode_command(&command).unwrap();
        let decoded = decode_command(&encoded).unwrap();
        assert_eq!(encode_command(&decoded).unwrap(), encoded);
        if matches!(
            command,
            ContextCommand::RestorePayload { .. } | ContextCommand::Continue { .. }
        ) {
            assert!(
                String::from_utf8(encoded)
                    .unwrap()
                    .ends_with(&format!("\"receipt\":\"{RECEIPT}\"}}"))
            );
        }
    }
}

#[test]
fn control_events_roundtrip_but_wrong_direction_and_ordinary_event_conversion_fail() {
    let digest = ContextDigest::of(b"fixture");
    let events = [
        ContextEvent::RestorePrepared {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            format_digest: digest,
            configuration_digest: digest,
        },
        ContextEvent::Restored {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            payload_digest: digest,
            format_digest: digest,
            configuration_digest: digest,
            receipt: receipt(),
        },
        ContextEvent::ContinuationAccepted {
            attempt_id: id("attempt"),
            restore_id: id("restore"),
            continuation_id: id("continue"),
            input_id: id("input"),
            input_digest: digest,
            receipt: receipt(),
        },
        ContextEvent::Failed {
            attempt_id: id("attempt"),
            operation_id: id("restore"),
            code: ContextFailure::Validation,
        },
    ];
    for event in events {
        let encoded = encode_event(&event).unwrap();
        assert_eq!(
            encode_event(&decode_event(&encoded).unwrap()).unwrap(),
            encoded
        );
        assert!(serde_json::from_slice::<foundry_contracts::AdapterEvent>(&encoded).is_err());
    }
    assert!(decode_command(&encode_event(&candidate(vec![1])).unwrap()).is_err());
    assert!(
        decode_event(
            &encode_command(&ContextCommand::Release {
                attempt_id: id("a"),
                operation_id: id("o")
            })
            .unwrap()
        )
        .is_err()
    );
}

#[test]
fn wrong_digest_length_and_noncanonical_base64_fail_without_leaking_payload() {
    for (field, value) in [
        ("payload_digest", json!("0".repeat(64))),
        ("payload_length", json!(1)),
        ("payload_length", json!(0)),
        ("payload_length", json!(MAX_PAYLOAD_BYTES + 1)),
        ("payload_base64", json!("YR==")),
        ("payload_base64", json!("YQ")),
        ("payload_base64", json!("YQ==\n")),
        ("payload_base64", json!("A".repeat(MAX_BASE64_BYTES + 1))),
    ] {
        let mut value_frame = candidate_value();
        value_frame[field] = value;
        let error = decode_event(&bytes(&value_frame)).err().unwrap();
        assert!(!format!("{error:?} {error}").contains("private-sentinel"));
    }
}

#[test]
fn duplicate_and_unknown_fields_fail_at_every_metadata_level() {
    let original = String::from_utf8(encode_event(&candidate(vec![1])).unwrap()).unwrap();
    for (needle, replacement) in [
        (
            "\"type\":\"context_candidate\"",
            "\"type\":\"text\",\"type\":\"context_candidate\"",
        ),
        (
            "\"attempt_id\":\"attempt\"",
            "\"attempt_id\":\"attempt\",\"attempt_id\":\"attempt\"",
        ),
        (
            "\"profile\":\"fixture\"",
            "\"profile\":\"fixture\",\"profile\":\"fixture\"",
        ),
        (
            "\"completed_turn_id\":\"turn\"",
            "\"completed_turn_id\":\"turn\",\"completed_turn_id\":\"turn\"",
        ),
        (
            "\"input_id\":\"input\"",
            "\"input_id\":\"input\",\"input_id\":\"input\"",
        ),
    ] {
        assert!(original.contains(needle));
        assert!(decode_event(original.replace(needle, replacement).as_bytes()).is_err());
    }
    for path in [
        vec![],
        vec!["format"],
        vec!["coverage"],
        vec!["coverage", "inputs", "0"],
    ] {
        let mut value = candidate_value();
        let mut target = &mut value;
        for key in path {
            target = if key == "0" {
                &mut target[0]
            } else {
                &mut target[key]
            };
        }
        target["unrecognized"] = json!({"private":"sentinel"});
        assert!(decode_event(&bytes(&value)).is_err());
    }
    let mut value = candidate_value();
    value["coverage"]["results"] = json!([{
        "attempt_id":"attempt","call_id":"call","result_digest":"0".repeat(64),"native_ids":["r"],"unknown":true
    }]);
    assert!(decode_event(&bytes(&value)).is_err());
}

#[test]
fn identifiers_digests_and_receipts_are_strict() {
    for value in ["", " ", "line\nbreak", &"x".repeat(129)] {
        assert!(ContextId::new(value).is_err());
    }
    assert!(ContextId::new("x".repeat(128)).is_ok());
    for value in ["", &"A".repeat(64), &"0".repeat(63), &"z".repeat(64)] {
        assert!(ContextDigest::parse(value).is_err());
    }
    assert_eq!(
        ContextDigest::of(b"abc").to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    for value in [
        "",
        "guessed",
        "01234567-89ab-1cde-8f01-23456789abcd",
        "01234567-89ab-4cde-0f01-23456789abcd",
    ] {
        assert!(ContextReceipt::new(value).is_err());
    }
}

#[test]
fn framing_metadata_and_continuation_limits_fail_closed() {
    let value = encode_event(&candidate(vec![1])).unwrap();
    assert!(decode_event(&value[..value.len() - 1]).is_err());
    let mut trailing = value.clone();
    trailing.extend_from_slice(b"{}");
    assert!(decode_event(&trailing).is_err());
    assert!(matches!(
        decode_event(&vec![b' '; foundry_contracts::MAX_FRAME_BYTES + 1]),
        Err(ContextError::Limit)
    ));
    let mut value = candidate_value();
    value["coverage"]["inputs"]=json!((0..1024).map(|i|json!({"input_id":format!("input-{i}"),"input_digest":"0".repeat(64),"native_ids":["r".repeat(128)]})).collect::<Vec<_>>());
    assert!(matches!(
        decode_event(&bytes(&value)),
        Err(ContextError::Limit)
    ));
    let mut command = ContextCommand::Continue {
        attempt_id: id("a"),
        restore_id: id("r"),
        continuation_id: id("c"),
        input_id: id("i"),
        input_digest: ContextDigest::of(b"different"),
        text: "private-sentinel".into(),
        receipt: receipt(),
    };
    assert!(matches!(
        encode_command(&command),
        Err(ContextError::Integrity)
    ));
    if let ContextCommand::Continue {
        input_digest, text, ..
    } = &mut command
    {
        *text = "x".repeat(foundry_contracts::MAX_PROMPT_BYTES);
        *input_digest = ContextDigest::of(text.as_bytes());
    }
    let bytes = encode_command(&command).unwrap();
    assert!(decode_command(&bytes).is_ok());
    if let ContextCommand::Continue {
        text, input_digest, ..
    } = &mut command
    {
        text.push('x');
        *input_digest = ContextDigest::of(text.as_bytes());
    }
    assert!(matches!(encode_command(&command), Err(ContextError::Limit)));
}
