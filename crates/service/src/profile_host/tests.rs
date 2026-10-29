use super::{Blob, Error, facts, json};
use foundry_store::{
    CaptureSnapshot, CaptureToolEvidence, CheckpointCut, CoverageManifest, ToolOutcome,
};
use serde_json::json;

#[test]
fn unresolved_tool_is_rejected_before_host_projection() {
    let mut snapshot = CaptureSnapshot {
        head: None,
        cut: CheckpointCut {
            work_revision: 1,
            event_seq: 1,
            event_digest: [1; 32],
            input_digest: [2; 32],
            tool_digest: [3; 32],
        },
        coverage: CoverageManifest {
            message_ids: vec![],
            tool_call_ids: vec!["call".into()],
        },
        prompt_digest: [4; 32],
        prompt_bytes: 1,
        messages: vec![],
        tools: vec![CaptureToolEvidence {
            call_id: "call".into(),
            attempt_id: "attempt".into(),
            tool: "read".into(),
            input_digest: [5; 32],
            outcome: ToolOutcome::Uncertain,
            output_digest: None,
        }],
    };
    assert!(matches!(facts::snapshot(&snapshot), Err(Error::Invalid)));
    snapshot.tools[0].outcome = ToolOutcome::Failed;
    assert_eq!(
        facts::snapshot(&snapshot).unwrap()["tools"][0]["outcome"],
        "failed"
    );
    snapshot.tools[0].outcome = ToolOutcome::Success;
    assert!(matches!(facts::snapshot(&snapshot), Err(Error::Invalid)));
    snapshot.tools[0].output_digest = Some([6; 32]);
    assert_eq!(
        facts::snapshot(&snapshot).unwrap()["tools"][0]["outcome"],
        "success"
    );
}

#[test]
fn aggregate_blob_and_encoded_byte_budgets_reject_excess() {
    let blob = json!({"bytes":1048576,"sha256":"00".repeat(32),"base64":""});
    let bytes = serde_json::to_vec(&vec![blob; 5]).unwrap();
    assert!(json::decode(&bytes, 8 * 1048576).is_err());
    assert!(json::encode(&"\n", 3).is_err());
    assert_eq!(json::encode(&"\n", 4).unwrap(), b"\"\\n\"");
}

#[test]
fn protocol_rejects_ambiguous_or_unbounded_values() {
    for bytes in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"a":1,"\u0061":2}"#,
        b"1.0",
        b"1e0",
        b"9007199254740992",
        b"-9007199254740992",
        b"{}{}",
    ] {
        assert!(json::decode(bytes, 65536).is_err());
    }
    let array = serde_json::to_vec(&vec![0; 1025]).unwrap();
    assert!(json::decode(&array, 65536).is_err());
    let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
    assert!(json::decode(deep.as_bytes(), 65536).is_err());
    assert!(json::decode(b"null", 3).is_err());
    assert_eq!(
        json::decode(b" {\"a\":-1} \n", 65536).unwrap(),
        json!({"a":-1})
    );
}

#[test]
fn blob_requires_exact_length_hash_and_canonical_padding() {
    let blob = Blob::new(b"a", 1).unwrap();
    assert_eq!(blob.decode(1).unwrap(), b"a");
    assert!(blob.decode(0).is_err());
    let value = serde_json::to_value(blob).unwrap();
    for (field, replacement) in [
        ("bytes", json!(0)),
        ("base64", json!("YQ")),
        ("base64", json!("Yg==")),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        let changed: Blob = serde_json::from_value(changed).unwrap();
        assert!(changed.decode(16).is_err());
    }
}
