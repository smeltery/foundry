use foundry_store::{
    ContextCheckpointId, RestoreContinueCodec, RestoreContinueIntentV1, RestoreContinueMode,
};
#[test]
fn independently_pinned_intent_digest() {
    let intent = RestoreContinueIntentV1 {
        session_id: "11111111-1111-4111-8111-111111111111".into(),
        restore_id: "22222222-2222-4222-8222-222222222222".into(),
        load_id: "33333333-3333-4333-8333-333333333333".into(),
        continuation_id: "fixture-continuation".into(),
        message_id: "fixture-message".into(),
        baseline_checkpoint_id: ContextCheckpointId::parse("44444444-4444-4444-8444-444444444444")
            .unwrap(),
        baseline_record_digest: [
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ],
        baseline_witness_digest: [
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2,
        ],
        baseline_validation_binding: [
            3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
            3, 3, 3,
        ],
        configuration_digest: [
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4,
        ],
        message_text_digest: [
            37, 32, 25, 101, 87, 137, 91, 188, 164, 22, 79, 150, 36, 69, 219, 93, 250, 130, 94, 85,
            248, 56, 58, 121, 250, 230, 183, 69, 189, 141, 219, 155,
        ],
        tail_receipt_digest: [
            111, 158, 42, 174, 64, 214, 89, 153, 181, 208, 142, 125, 75, 194, 193, 73, 67, 86, 11,
            222, 45, 119, 60, 64, 159, 31, 207, 38, 179, 208, 25, 65,
        ],
        message_sequence: 7,
        message_text_bytes: 12,
        mode: RestoreContinueMode::QueuedInstruction,
        codec: RestoreContinueCodec::RestoreContinueV2,
    };
    let actual: String = intent
        .delivery_digest()
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        actual,
        "ccc515379c9630dae37fda5614017d3c9b1777fa29f5404516c28f5c03c544fe"
    );
}
