#![cfg(unix)]
mod context_capture;
#[path = "../src/context_commit.rs"]
mod context_commit;
#[path = "../src/context_cut.rs"]
mod context_cut;
use context_capture::{fresh, profile};
use context_commit::prepare_finalization;
use context_cut::prepare_cut;
use foundry_contracts::{CreateSession, Lifecycle, Outcome, context::ContextDigest};
use foundry_service::{Result, ServiceError};
use foundry_store::{ContextFormat, CoverageManifest, OpaqueCheckpointInput, Store, ToolOutcome};

#[test]
fn actual_runtime_prepared_preserves_bytes_terminal_annotation_and_request_identity() {
    let fixture = fresh("private result\n\0 supplementary 🦀");
    let before = fixture.store.get(&fixture.fence.session_id).unwrap();
    let cut = fixture.evidence.snapshot().cut.clone();
    let pending = prepare_finalization(
        fixture.prepared,
        &fixture.fence,
        &fixture.evidence,
        fixture.expected,
    )
    .unwrap();
    assert_eq!(pending.request_id(), "capture-request");
    let digest = pending.request_digest();
    let (input, terminal, _cleanup) = pending.into_parts();
    assert_eq!(input.payload, b"\0opaque\xff\n\x80fixture");
    assert_eq!(input.cut, cut);
    assert_eq!(input.configuration_digest, profile().configuration_digest);
    assert_eq!(
        input.format,
        ContextFormat {
            format_id: "stored-format".into(),
            format_version: 5,
            profile_id: "stored-profile".into(),
            binding_id: "binding".into(),
            binding_version: "binding-v7".into()
        }
    );
    assert!(input.expected_head.is_none());
    assert_eq!(terminal.lifecycle, Lifecycle::Exit);
    assert_eq!(terminal.outcome, Some(Outcome::ActionRequired));
    assert_eq!(
        terminal.result.as_deref(),
        Some("private result\n\0 supplementary 🦀")
    );
    assert_eq!(
        input.producer_validation,
        Some(serde_json::json!({
            "schema_version": 1, "wire_format": {"profile":"wire-profile", "format_version":"wire-v3", "producer_build":"fixture-build"},
            "validator_id":"synthetic-protocol-fixture", "validator_version":"1", "completed_turn_id":"turn",
            "validation_binding":fixture.expected.to_hex(),
        }))
    );
    assert_eq!(
        digest,
        input
            .finalization_request_digest(&fixture.fence, &terminal)
            .unwrap()
    );
    assert_eq!(
        fixture.store.get(&fixture.fence.session_id).unwrap(),
        before
    );
    let mut store = fixture.store;
    let receipt = store
        .finish_with_opaque_checkpoint(&fixture.fence, input, terminal)
        .unwrap();
    assert_eq!(receipt.checkpoint.request_digest, digest);
    assert_eq!(
        store
            .read_opaque_checkpoint(&fixture.fence.session_id, &receipt.checkpoint.id)
            .unwrap()
            .payload(),
        b"\0opaque\xff\n\x80fixture"
    );
    drop(fixture.directory);
}

#[test]
fn independently_cached_validation_cut_and_owner_mismatches_reject_without_writes() {
    for mismatch in ["validation", "cut", "owner"] {
        let fixture = fresh("result");
        let before = fixture.store.get(&fixture.fence.session_id).unwrap();
        let mut fence = fixture.fence.clone();
        let mut evidence = fixture.evidence;
        let mut expected = fixture.expected;
        match mismatch {
            "validation" => expected = ContextDigest::of(b"different job"),
            "cut" => {
                evidence = prepare_cut(
                    &fixture.store,
                    &fence,
                    "different-request".into(),
                    profile(),
                )
                .unwrap()
                .1
            }
            _ => fence.attempt_id = uuid::Uuid::new_v4().to_string(),
        }
        let result = prepare_finalization(fixture.prepared, &fence, &evidence, expected);
        assert!(matches!(result, Err(ServiceError::Coordination(_))));
        assert_eq!(
            fixture.store.get(&fixture.fence.session_id).unwrap(),
            before
        );
        assert!(
            fixture
                .store
                .checkpoint_head(&fixture.fence.session_id)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn combined_store_metadata_limit_is_not_the_standalone_terminal_limit() {
    for result in ["x".repeat(16384), "\0".repeat(3000)] {
        let fixture = fresh(&result);
        let before = fixture.store.get(&fixture.fence.session_id).unwrap();
        assert!(matches!(
            prepare_finalization(
                fixture.prepared,
                &fixture.fence,
                &fixture.evidence,
                fixture.expected
            ),
            Err(ServiceError::Store(_))
        ));
        assert_eq!(
            fixture.store.get(&fixture.fence.session_id).unwrap(),
            before
        );
        assert!(
            fixture
                .store
                .checkpoint_head(&fixture.fence.session_id)
                .unwrap()
                .is_none()
        );
    }
    let fixture = fresh(&"x".repeat(14000));
    assert!(
        prepare_finalization(
            fixture.prepared,
            &fixture.fence,
            &fixture.evidence,
            fixture.expected
        )
        .is_ok()
    );
}

#[test]
fn one_store_snapshot_maps_all_facts_and_retains_prior_head_and_digest_evidence() {
    use sha2::{Digest, Sha256};
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("db")).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "prompt\n\0🦀".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let (initial, initial_evidence) =
        prepare_cut(&store, &fence, "initial".into(), profile()).unwrap();
    assert!(initial.identity().expected_head.is_none());
    assert!(initial_evidence.snapshot().head.is_none());
    assert!(initial_evidence.events().is_empty());
    let prior = store
        .commit_opaque_checkpoint(
            &fence,
            OpaqueCheckpointInput {
                request_id: "prior".into(),
                expected_head: None,
                cut: store.checkpoint_cut(&fence).unwrap(),
                format: ContextFormat {
                    format_id: "previous".into(),
                    format_version: 1,
                    profile_id: "previous".into(),
                    binding_id: "previous".into(),
                    binding_version: "1".into(),
                },
                configuration_digest: [99; 32],
                coverage: CoverageManifest {
                    message_ids: vec![],
                    tool_call_ids: vec![],
                },
                payload: b"prior opaque".to_vec(),
                producer_validation: None,
            },
        )
        .unwrap();
    for (id, text) in [("z-message", "last\0"), ("a-message", "first🦀")] {
        store.enqueue_message(&session.id, id, text).unwrap();
        store.claim_next_message(&fence).unwrap().unwrap();
        store.append_event(&fence, &format!("message-accepted:{id}"), 0, "message_accepted",
            serde_json::json!({"type":"message_accepted","attempt_id":fence.attempt_id,"message_id":id,"receipt":"fixture-private-receipt"})).unwrap();
        store.acknowledge_message(&fence, id).unwrap();
    }
    store
        .claim_tool(&fence, "z-tool", "read", [43; 32])
        .unwrap();
    store
        .finish_tool(&fence, "z-tool", ToolOutcome::Success, Some([44; 32]))
        .unwrap();
    store
        .claim_tool(&fence, "a-tool", "write", [45; 32])
        .unwrap();
    store
        .finish_tool(&fence, "a-tool", ToolOutcome::Failed, None)
        .unwrap();
    store
        .append_event(
            &fence,
            "event",
            0,
            "fixture",
            serde_json::json!({"done":true}),
        )
        .unwrap();
    let snapshot = store.capture_snapshot(&fence).unwrap();
    let (cut, evidence) = prepare_cut(&store, &fence, "mapped".into(), profile()).unwrap();
    assert_eq!(evidence.snapshot(), &snapshot);
    let events = evidence.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].subject, "z-message");
    assert_eq!(events[1].subject, "a-message");
    assert!(
        events
            .iter()
            .all(|event| event.sequence <= snapshot.cut.event_seq)
    );
    assert!(evidence.cut_binding() == cut.binding_digest());
    assert_eq!(cut.identity().session_id, session.id);
    assert_eq!(cut.identity().attempt_id, fence.attempt_id);
    assert_eq!(cut.identity().request_id, "mapped");
    let head = cut.identity().expected_head.as_ref().unwrap();
    assert_eq!(head.checkpoint_id, prior.id.as_str());
    assert_eq!(head.generation, prior.generation);
    assert_eq!(
        evidence.snapshot().head.as_ref().unwrap().record_digest,
        prior.record_digest
    );
    assert_eq!(cut.facts().work_revision, snapshot.cut.work_revision);
    assert_eq!(cut.facts().event_seq, snapshot.cut.event_seq);
    assert_eq!(cut.facts().event_digest, snapshot.cut.event_digest);
    assert_eq!(cut.facts().input_digest, snapshot.cut.input_digest);
    assert_eq!(cut.facts().tool_digest, snapshot.cut.tool_digest);
    assert_eq!(cut.coverage().message_ids, ["a-message", "z-message"]);
    assert_eq!(cut.coverage().tool_call_ids, ["a-tool", "z-tool"]);
    assert_eq!(
        snapshot.prompt_digest,
        <[u8; 32]>::from(Sha256::digest(session.prompt.as_bytes()))
    );
    assert_eq!(snapshot.messages[0].sequence, 2);
    assert_eq!(snapshot.messages[0].attempt_id, fence.attempt_id);
    assert_eq!(
        snapshot.messages[0].text_digest,
        <[u8; 32]>::from(Sha256::digest("first🦀".as_bytes()))
    );
    assert_eq!(snapshot.tools[0].outcome, ToolOutcome::Failed);
    assert_eq!(snapshot.tools[0].output_digest, None);
    assert_eq!(snapshot.tools[1].output_digest, Some([44; 32]));
    let selected = cut.profile();
    assert_eq!(selected.stored.format_id, "stored-format");
    assert_eq!(selected.stored.format_version, 5);
    assert_eq!(selected.stored.profile_id, "stored-profile");
    assert_eq!(selected.stored.binding_id, "binding");
    assert_eq!(selected.stored.binding_version, "binding-v7");
    assert_eq!(selected.wire.profile.as_str(), "wire-profile");
    assert_eq!(selected.wire.format_version.as_str(), "wire-v3");
    assert_eq!(selected.wire.producer_build.as_str(), "fixture-build");
    assert_eq!(
        selected.configuration_digest,
        profile().configuration_digest
    );
    assert_eq!(
        selected.configuration_wire_digest().unwrap().to_hex(),
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    );
}
