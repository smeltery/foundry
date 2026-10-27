use super::{tests::*, *};
use foundry_contracts::{Lifecycle, Outcome, context::ContextPayload};

#[test]
fn terminal_substitution_changes_exact_validation_authority() {
    let correlation = correlation();
    let make = |outcome, text| {
        CaptureValidationJob::new(
            &correlation,
            cut(),
            candidate(ContextPayload::new(vec![1]).unwrap()),
            &challenge(),
            trace(),
            CaptureTerminal::new(Lifecycle::Exit, outcome, text).unwrap(),
        )
        .unwrap()
    };
    let original = make(Outcome::Success, "private-original");
    let expected = original.binding_digest();
    for substituted in [
        make(Outcome::Failure, "private-original"),
        make(Outcome::Success, "other"),
    ] {
        let verified = substituted
            .accept_from_trusted_owner(attestation("turn"))
            .unwrap();
        assert!(verified.binding_digest() != expected);
    }
    assert_eq!(original.terminal().lifecycle(), Lifecycle::Exit);
    let verified = original
        .accept_from_trusted_owner(attestation("turn"))
        .unwrap();
    assert!(verified.binding_digest() == expected);
    let (_, _, _, terminal) = verified.into_owner_parts();
    assert_eq!(terminal.result(), "private-original");
    assert_eq!(terminal.outcome(), Outcome::Success);
    assert!(CaptureTerminal::new(Lifecycle::Suspended, Outcome::Success, "x").is_err());
    assert!(
        CaptureTerminal::new(
            Lifecycle::Exit,
            Outcome::Success,
            &"x".repeat(MAX_TERMINAL_RESULT_BYTES + 1)
        )
        .is_err()
    );
}

#[test]
fn typed_helper_result_is_bounded_before_serialization_and_exact_on_wire() {
    use foundry_tools::host::{Effect, Outcome as ToolOutcome};
    let result = ToolOutcome::Error {
        code: foundry_tools::host::ErrorCode::Denied,
        effect: Effect::None,
    };
    let mut trace = trace();
    let reserved = trace
        .reserve_helper_result("attempt", "call", [9; 32], &result)
        .unwrap();
    assert_eq!(reserved.bytes(), serde_json::to_vec(&result).unwrap());
    reserved.commit_enqueued();
    assert!(matches!(trace.entries()[1].kind(), TraceKind::ToolResult(
        ResultProvenance::PersistedHelper { ledger_output_digest }) if *ledger_output_digest == [9;32]));
    let oversized = ToolOutcome::Ok {
        output: serde_json::json!("x".repeat(MAX_TRACE_BYTES)),
    };
    let used = trace.used_bytes();
    assert!(matches!(
        trace.reserve_helper_result("attempt", "large", [9; 32], &oversized),
        Err(CaptureError::Limit)
    ));
    assert_eq!(trace.used_bytes(), used);
    assert_eq!(trace.entries().len(), 2);
}
