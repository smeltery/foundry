use foundry_contracts::CreateSession;
use foundry_store::{AttemptFence, ContextFormat, CoverageManifest, OpaqueCheckpointInput, Store};

pub fn session(store: &mut Store) -> String {
    store
        .create(CreateSession {
            prompt: "opaque storage fixture".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
        .id
}

pub fn input(store: &Store, fence: &AttemptFence, request: &str) -> OpaqueCheckpointInput {
    OpaqueCheckpointInput {
        request_id: request.into(),
        expected_head: None,
        cut: store.checkpoint_cut(fence).unwrap(),
        format: ContextFormat {
            format_id: "opaque.fixture".into(),
            format_version: 1,
            profile_id: "test".into(),
            binding_id: "fixture".into(),
            binding_version: "1".into(),
        },
        configuration_digest: [7; 32],
        coverage: CoverageManifest {
            message_ids: vec![],
            tool_call_ids: vec![],
        },
        payload: b"\0private opaque bytes\xff".to_vec(),
        producer_validation: None,
    }
}
