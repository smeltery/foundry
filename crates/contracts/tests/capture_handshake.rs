use foundry_contracts::{AdapterCommand, AdapterEvent, context::*};
use serde_json::{Value, json};

fn frame(tag: &str) -> Value {
    json!({"type":tag,"attempt_id":"attempt","capture_id":"capture",
        "format_digest":"11".repeat(32),"configuration_digest":"22".repeat(32),
        "challenge":"01234567-89ab-4cde-8f01-23456789abcd"})
}

#[test]
fn capture_handshake_is_private_directional_and_preserves_all_bindings() {
    assert_eq!(CAPTURE_CAPABILITY, "context_capture_v1");
    for tag in ["context_capture_begin", "context_capture_ready"] {
        let expected = frame(tag);
        let bytes = serde_json::to_vec(&expected).unwrap();
        assert!(is_context_frame(&bytes).unwrap());
        assert!(serde_json::from_slice::<AdapterCommand>(&bytes).is_err());
        assert!(serde_json::from_slice::<AdapterEvent>(&bytes).is_err());
        let encoded = if tag == "context_capture_begin" {
            assert!(decode_event(&bytes).is_err());
            let command = decode_command(&bytes).unwrap();
            assert!(matches!(command, ContextCommand::CaptureBegin { .. }));
            assert_eq!(command.attempt_id().as_str(), "attempt");
            encode_command(&command).unwrap()
        } else {
            assert!(decode_command(&bytes).is_err());
            let event = decode_event(&bytes).unwrap();
            assert!(matches!(event, ContextEvent::CaptureReady { .. }));
            assert_eq!(event.attempt_id().as_str(), "attempt");
            encode_event(&event).unwrap()
        };
        assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), expected);
    }
}

#[test]
fn handshake_rejects_duplicate_unknown_missing_and_invalid_fields() {
    for tag in ["context_capture_begin", "context_capture_ready"] {
        let reject = |bytes: &[u8]| {
            assert!(decode_command(bytes).is_err());
            assert!(decode_event(bytes).is_err());
        };
        let original = frame(tag);
        for key in original.as_object().unwrap().keys() {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(key);
            reject(&serde_json::to_vec(&missing).unwrap());
            let encoded = serde_json::to_string(&original).unwrap();
            let duplicate = format!("{{\"{key}\":{},{}", original[key], &encoded[1..]);
            reject(duplicate.as_bytes());
        }
        for (key, value) in [
            ("extra", json!(true)),
            ("attempt_id", json!(" ")),
            ("capture_id", json!("x\n")),
            ("capture_id", json!("é".repeat(65))),
            ("format_digest", json!("AA".repeat(32))),
            ("configuration_digest", json!("00")),
            ("challenge", json!("literal-not-a-uuid")),
        ] {
            let mut invalid = original.clone();
            invalid[key] = value;
            reject(&serde_json::to_vec(&invalid).unwrap());
        }
    }
}
