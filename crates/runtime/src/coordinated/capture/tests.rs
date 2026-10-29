use super::*;
use crate::coordinated::{CheckpointId, capture_owner::*};
use foundry_contracts::{MAX_PROMPT_BYTES, context::*};
use serde_json::json;

fn id(value: &str) -> ContextId {
    ContextId::new(value).unwrap()
}
fn inputs() -> (
    CoreCutIdentity,
    CoreCutFacts,
    ProfileBinding,
    DurableCoverage,
) {
    (
        CoreCutIdentity {
            session_id: "session".into(),
            attempt_id: "attempt".into(),
            request_id: "request".into(),
            expected_head: Some(HeadIdentity {
                checkpoint_id: "01234567-89ab-4cde-8f01-23456789abcd".into(),
                generation: 2,
            }),
        },
        CoreCutFacts {
            work_revision: 3,
            event_seq: 4,
            event_digest: [1; 32],
            input_digest: [2; 32],
            tool_digest: [3; 32],
        },
        ProfileBinding {
            stored: StoredFormat {
                format_id: "stored-format".into(),
                format_version: 7,
                profile_id: "stored-profile".into(),
                binding_id: "binding".into(),
                binding_version: "v8".into(),
            },
            wire: ContextFormat {
                profile: id("wire-profile"),
                format_version: id("native-2"),
                producer_build: id("producer-3"),
            },
            configuration_digest: [4; 32],
        },
        DurableCoverage {
            message_ids: vec![],
            tool_call_ids: vec![],
        },
    )
}
pub(super) fn cut() -> CaptureCut {
    let (identity, facts, profile, coverage) = inputs();
    CaptureCut::from_trusted_owner(identity, facts, profile, coverage).unwrap()
}
pub(super) fn trace() -> DeliveredTrace {
    let mut trace = DeliveredTrace::new();
    trace
        .reserve_prompt("session", "attempt", "prompt")
        .unwrap()
        .commit_enqueued();
    trace
}

#[test]
fn cut_preserves_every_stored_format_field_without_inventing_wire_mapping() {
    let cut = cut();
    assert_eq!(cut.profile().stored.format_id, "stored-format");
    assert_eq!(cut.profile().stored.format_version, 7);
    assert_eq!(cut.profile().stored.profile_id, "stored-profile");
    assert_eq!(cut.profile().stored.binding_id, "binding");
    assert_eq!(cut.profile().stored.binding_version, "v8");
    assert_eq!(cut.profile().wire.format_version.as_str(), "native-2");
    let original = cut.binding_digest();
    for change in 0..10 {
        let (mut identity, mut facts, mut profile, mut coverage) = inputs();
        match change {
            0 => identity.request_id = "other".into(),
            1 => identity.expected_head.as_mut().unwrap().generation += 1,
            2 => facts.work_revision += 1,
            3 => facts.event_seq += 1,
            4 => facts.event_digest[0] += 1,
            5 => facts.input_digest[0] += 1,
            6 => facts.tool_digest[0] += 1,
            7 => profile.stored.binding_version = "other".into(),
            8 => profile.configuration_digest[0] += 1,
            _ => coverage.tool_call_ids.push("tool".into()),
        }
        assert!(
            CaptureCut::from_trusted_owner(identity, facts, profile, coverage)
                .unwrap()
                .binding_digest()
                != original
        );
    }
}

#[test]
fn cut_rejects_bad_identity_integer_coverage_and_aggregate_encoding() {
    for change in 0..8 {
        let (mut identity, mut facts, mut profile, mut coverage) = inputs();
        match change {
            0 => identity.session_id = "bad\n".into(),
            1 => identity.expected_head.as_mut().unwrap().checkpoint_id = "invalid".into(),
            2 => facts.work_revision = u64::MAX,
            3 => profile.stored.format_version = 0,
            4 => coverage.message_ids = vec!["x".into(), "x".into()],
            5 => coverage.tool_call_ids = vec!["z".into(), "a".into()],
            6 => coverage.message_ids = (0..1025).map(|i| format!("{i:04}")).collect(),
            _ => {
                coverage.message_ids = (0..600)
                    .map(|i| format!("{i:04}{}", "x".repeat(124)))
                    .collect()
            }
        }
        assert!(CaptureCut::from_trusted_owner(identity, facts, profile, coverage).is_err());
    }
}

#[test]
fn prompt_counts_and_reservations_drop_without_committing_or_consuming_capacity() {
    let mut trace = DeliveredTrace::new();
    assert!(!trace.is_quiescent());
    assert!(trace.reserve_instruction("attempt", "m", "text").is_err());
    {
        let reserved = trace
            .reserve_prompt("session", "attempt", "prompt")
            .unwrap();
        assert_eq!(reserved.bytes(), b"prompt");
    }
    assert_eq!(trace.used_bytes(), 0);
    assert!(trace.entries().is_empty());
    trace
        .reserve_prompt("session", "attempt", "prompt")
        .unwrap()
        .commit_enqueued();
    assert_eq!(trace.entries().len(), 1);
    assert_eq!(trace.used_bytes(), TRACE_ENTRY_FIXED_BYTES + 7 + 7 + 6);
    assert!(
        trace
            .reserve_prompt("session", "attempt", "prompt")
            .is_err()
    );
    assert!(trace.reserve_instruction("foreign", "m", "text").is_err());
}

#[test]
fn receipt_alone_never_makes_an_instruction_durable() {
    let mut trace = trace();
    trace
        .reserve_instruction("attempt", "m", "exact input")
        .unwrap()
        .commit_enqueued();
    let job = CheckpointId::new();
    assert_eq!(
        trace.confirm_instruction_persisted("attempt", "m", job),
        Err(CaptureError::Pending)
    );
    trace
        .observe_instruction_receipt("attempt", "m", job)
        .unwrap();
    trace
        .observe_instruction_receipt("attempt", "m", job)
        .unwrap();
    assert!(!trace.is_quiescent());
    assert_eq!(
        trace.confirm_instruction_persisted("attempt", "m", CheckpointId::new()),
        Err(CaptureError::Pending)
    );
    assert_eq!(
        trace.confirm_instruction_persisted("foreign", "m", job),
        Err(CaptureError::Binding)
    );
    trace
        .confirm_instruction_persisted("attempt", "m", job)
        .unwrap();
    assert!(trace.is_quiescent());
    trace
        .observe_instruction_receipt("attempt", "m", CheckpointId::new())
        .unwrap();
    assert_eq!(trace.entries().len(), 2);
    assert!(
        trace
            .reserve_instruction("attempt", "m", "different")
            .is_err()
    );
}

#[test]
fn aggregate_trace_byte_limit_is_inclusive_and_failure_never_changes_history() {
    let mut trace = trace();
    for i in 0..3 {
        trace
            .reserve_instruction("attempt", &format!("m{i}"), &"x".repeat(MAX_PROMPT_BYTES))
            .unwrap()
            .commit_enqueued();
    }
    let length = MAX_TRACE_BYTES
        - trace.used_bytes()
        - TRACE_ENTRY_FIXED_BYTES
        - "attempt".len()
        - "last".len();
    trace
        .reserve_instruction("attempt", "last", &"x".repeat(length))
        .unwrap()
        .commit_enqueued();
    assert_eq!(trace.used_bytes(), MAX_TRACE_BYTES);
    assert!(matches!(
        trace.reserve_instruction("attempt", "overflow", "x"),
        Err(CaptureError::Limit)
    ));
    assert_eq!(trace.entries().len(), 5);
    assert_eq!(trace.used_bytes(), MAX_TRACE_BYTES);
}

#[test]
fn prompt_consumes_one_of_the_1024_trace_entries() {
    let mut trace = trace();
    for i in 1..MAX_TRACE_ENTRIES {
        trace
            .reserve_instruction("attempt", &format!("m{i}"), "x")
            .unwrap()
            .commit_enqueued();
    }
    assert!(matches!(
        trace.reserve_instruction("attempt", "overflow", "x"),
        Err(CaptureError::Limit)
    ));
    assert_eq!(trace.entries().len(), MAX_TRACE_ENTRIES);
}

#[test]
fn results_keep_wire_bytes_separate_from_ledger_digest_and_include_denials() {
    let mut trace = trace();
    let result = json!({"status":"denied", "code":"disabled"});
    let body = serde_json::to_vec(&result).unwrap();
    trace
        .reserve_result("attempt", "deny", ResultProvenance::Denied, &result)
        .unwrap()
        .commit_enqueued();
    trace
        .reserve_result(
            "attempt",
            "helper",
            ResultProvenance::PersistedHelper {
                ledger_output_digest: [9; 32],
            },
            &result,
        )
        .unwrap()
        .commit_enqueued();
    assert_eq!(trace.entries()[1].bytes(), body);
    assert!(trace.entries()[1].wire_digest() == ContextDigest::of(&body));
    assert!(
        matches!(trace.entries()[2].kind(), TraceKind::ToolResult(ResultProvenance::PersistedHelper { ledger_output_digest }) if ledger_output_digest == &[9;32])
    );
    assert!(
        trace
            .reserve_result("attempt", "deny", ResultProvenance::Denied, &result)
            .is_err()
    );
    let before = trace.used_bytes();
    assert!(matches!(
        trace.reserve_result(
            "attempt",
            "large",
            ResultProvenance::Denied,
            &json!("x".repeat(MAX_TRACE_BYTES))
        ),
        Err(CaptureError::Limit)
    ));
    assert_eq!(trace.used_bytes(), before);
    assert_eq!(trace.entries().len(), 3);
}

pub(super) fn correlation() -> CaptureCorrelation {
    CaptureCorrelation {
        job: CaptureJobId::fresh(),
        attempt_id: id("attempt"),
        capture_id: id("capture"),
    }
}
pub(super) fn challenge() -> ContextReceipt {
    ContextReceipt::new("01234567-89ab-4cde-8f01-23456789abcd").unwrap()
}
pub(super) fn candidate(payload: ContextPayload) -> ContextEvent {
    ContextEvent::Candidate {
        attempt_id: id("attempt"),
        capture_id: id("capture"),
        challenge: challenge(),
        format: inputs().2.wire,
        coverage: ContextCoverage {
            completed_turn_id: id("turn"),
            inputs: vec![],
            results: vec![],
        },
        payload,
    }
}
pub(super) fn attestation(turn: &str) -> OwnerValidationAttestation {
    OwnerValidationAttestation {
        validator_id: id("offline-fixture"),
        validator_version: id("1"),
        completed_turn_id: id(turn),
    }
}

#[test]
fn consuming_owner_attestation_preserves_exact_payload_without_claiming_native_proof() {
    let payload = ContextPayload::new(b"opaque-private-fixture".to_vec()).unwrap();
    let pointer = payload.as_bytes().as_ptr();
    let expected_correlation = correlation();
    let job = CaptureValidationJob::new(
        &expected_correlation,
        cut(),
        candidate(payload),
        &challenge(),
        trace(),
        terminal(),
    )
    .unwrap();
    assert!(job.correlation() == &expected_correlation);
    let binding = job.binding_digest();
    assert_eq!(job.candidate().payload().as_ptr(), pointer);
    // This is intentionally an owner assertion over an opaque fixture, not a validator.
    let verified = job.accept_from_trusted_owner(attestation("turn")).unwrap();
    assert!(verified.binding_digest() == binding);
    assert_eq!(verified.payload().as_ptr(), pointer);
    let (cut, payload, _, _) = verified.into_owner_parts();
    assert_eq!(payload.as_bytes(), b"opaque-private-fixture");
    assert_eq!(cut.profile().stored.binding_version, "v8");
}

#[test]
fn validation_binding_rejects_mismatches_and_unpersisted_instructions() {
    for mode in 0..4 {
        let mut event = candidate(ContextPayload::new(vec![1]).unwrap());
        let mut trace = trace();
        if let ContextEvent::Candidate {
            capture_id,
            format,
            challenge,
            ..
        } = &mut event
        {
            match mode {
                0 => *capture_id = id("wrong"),
                1 => format.producer_build = id("wrong"),
                2 => *challenge = ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
                _ => trace
                    .reserve_instruction("attempt", "m", "pending")
                    .unwrap()
                    .commit_enqueued(),
            }
        }
        assert!(
            CaptureValidationJob::new(
                &correlation(),
                cut(),
                event,
                &challenge(),
                trace,
                terminal()
            )
            .is_err()
        );
    }
    let make = |correlation: &CaptureCorrelation| {
        CaptureValidationJob::new(
            correlation,
            cut(),
            candidate(ContextPayload::new(vec![1]).unwrap()),
            &challenge(),
            trace(),
            terminal(),
        )
        .unwrap()
    };
    let one = make(&correlation());
    let two = make(&correlation());
    assert!(one.binding_digest() != two.binding_digest());
    assert!(
        one.accept_from_trusted_owner(attestation("wrong-turn"))
            .is_err()
    );
}

fn terminal() -> CaptureTerminal {
    CaptureTerminal::new(
        foundry_contracts::Lifecycle::Exit,
        foundry_contracts::Outcome::Success,
        "result",
    )
    .unwrap()
}
