use super::{support::Harness, *};
use crate::coordinated::{capture::*, capture_owner::*, *};
use foundry_contracts::{AdapterEvent, Lifecycle, Session, context::ContextFormat};
use std::{sync::mpsc, thread};

fn profile() -> ProfileBinding {
    let id = |v| ContextId::new(v).unwrap();
    ProfileBinding {
        stored: StoredFormat {
            format_id: "native".into(),
            format_version: 1,
            profile_id: "current".into(),
            binding_id: "binding".into(),
            binding_version: "1".into(),
        },
        wire: ContextFormat {
            profile: id("current"),
            format_version: id("1"),
            producer_build: id("fixture"),
        },
        configuration_digest: std::array::from_fn(|i| {
            u8::from_str_radix(
                &ContextDigest::of(b"effective-config").to_hex()[i * 2..i * 2 + 2],
                16,
            )
            .unwrap()
        }),
    }
}
fn cut(parent: String, mutation: &str) -> CaptureCut {
    let mut head = Some(HeadIdentity {
        checkpoint_id: parent,
        generation: 1,
    });
    let mut p = profile();
    match mutation {
        "missing_parent" => head = None,
        "wrong_parent" => head.as_mut().unwrap().checkpoint_id = uuid::Uuid::new_v4().to_string(),
        "generation" => head.as_mut().unwrap().generation += 1,
        "profile" => p.configuration_digest[0] += 1,
        _ => {}
    }
    CaptureCut::from_trusted_owner(
        CoreCutIdentity {
            session_id: "session".into(),
            attempt_id: "new".into(),
            request_id: "child".into(),
            expected_head: head,
        },
        CoreCutFacts {
            work_revision: 7,
            event_seq: 9,
            event_digest: [1; 32],
            input_digest: [2; 32],
            tool_digest: [3; 32],
        },
        p,
        DurableCoverage {
            message_ids: vec!["input".into()],
            tool_call_ids: vec!["call".into()],
        },
    )
    .unwrap()
}
fn scenario(mode: &str, mutation: &str) -> bool {
    let bad_cut = matches!(
        mutation,
        "missing_parent" | "wrong_parent" | "generation" | "profile"
    );
    let mut h = Harness::new(mode);
    let (_, digest) = h.grant();
    let job = h.accepted(digest);
    h.replies
        .send(ContinuationReply::Activated { job })
        .ok()
        .unwrap();
    assert!(h.pending.poll(&h.cancellation).unwrap());
    let admitted = h.pending.into_admitted().unwrap();
    let pid = admitted.process_id();
    let observer = admitted.cleanup_observer();
    let parent = admitted
        .binding
        .baseline
        .restored_identity()
        .unwrap()
        .checkpoint_id
        .as_str()
        .to_owned();
    let session = Session {
        id: "session".into(),
        prompt: "historical".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("new".into()),
        version: 2,
    };
    let (owner, endpoint) = CaptureOwner::channel();
    let (requests, received) = mpsc::sync_channel(2);
    let (reply_tx, replies) = mpsc::sync_channel(2);
    let (_messages, messages) = mpsc::channel();
    let spec = &h.fixture.spec;
    let cancellation = h.cancellation;
    let result = thread::scope(|scope| {
        let persistence = scope.spawn(move || {
            let mut count = 0;
            while let Ok(PersistenceRequest::Event { checkpoint, event }) = received.recv() {
                if let AdapterEvent::Settled { result, .. } = event {
                    assert!(result.is_empty());
                }
                count += 1;
                reply_tx
                    .send(PersistenceReply::EventCommitted { checkpoint })
                    .unwrap();
            }
            count
        });
        let runtime = scope.spawn(move || {
            admitted.run_restored_capture(
                spec,
                &session,
                &cancellation,
                &messages,
                RuntimePersistence { requests, replies },
                FreshCaptureOptions {
                    profile: profile(),
                    owner,
                    policy: None,
                    phase_timeout: Duration::from_millis(300),
                },
            )
        });
        let CaptureRequest::FreezeAdmission { correlation } = endpoint
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!()
        };
        endpoint
            .replies
            .send(CaptureReply::AdmissionFrozen { correlation })
            .ok()
            .unwrap();
        let CaptureRequest::PrepareCut { correlation } = endpoint
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!()
        };
        endpoint
            .replies
            .send(CaptureReply::CutPrepared {
                correlation,
                cut: Box::new(cut(parent, mutation)),
            })
            .ok()
            .unwrap();
        if !bad_cut
            && mode != "continue_capture_challenge"
            && mode != "continue_capture_payload"
            && mode != "continue_capture_stall"
            && mode != "continue_capture_duplicate"
        {
            let CaptureRequest::ValidateCandidate { correlation, job } = endpoint
                .requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0); // Export/validation before cleanup.
            let origin = job.restored_origin().unwrap();
            assert!(origin.historical_configuration_digest == ContextDigest::of(b"config"));
            assert!(
                origin.binding.effective_configuration_digest
                    == ContextDigest::of(b"effective-config")
            );
            assert!(
                origin.historical_configuration_digest
                    != origin.binding.effective_configuration_digest
            );
            assert!(
                job.cut().profile().configuration_wire_digest().unwrap()
                    == origin.binding.effective_configuration_digest
            );
            assert_eq!(
                origin
                    .binding
                    .baseline
                    .restored_identity()
                    .unwrap()
                    .original_prompt_attempt_id
                    .as_str(),
                "original"
            );
            assert_eq!(origin.continuation.attempt_id.as_str(), "new");
            assert_eq!(job.trace().entries().len(), 2);
            assert!(matches!(
                job.trace().entries()[0].kind(),
                TraceKind::Instruction
            ));
            assert_eq!(job.candidate().payload(), b"native-baseline-plus-new-delta");
            assert_eq!(job.terminal().result(), "done");
            if mutation == "cancel_validation" {
                h.cancel.send(CancelMode::Hard).unwrap();
            } else if mutation == "reject_validation" {
                endpoint
                    .replies
                    .send(CaptureReply::Rejected {
                        correlation,
                        code: CaptureRejection::Validation,
                    })
                    .ok()
                    .unwrap();
            } else {
                let verified = job
                    .accept_from_trusted_owner(OwnerValidationAttestation {
                        validator_id: ContextId::new("fixture-only").unwrap(),
                        validator_version: ContextId::new("1").unwrap(),
                        completed_turn_id: ContextId::new("turn").unwrap(),
                    })
                    .unwrap();
                endpoint
                    .replies
                    .send(CaptureReply::CandidateVerified {
                        correlation,
                        verified: Box::new(verified),
                    })
                    .ok()
                    .unwrap();
            }
        }
        let result = runtime.join().unwrap();
        assert_eq!(persistence.join().unwrap(), 3);
        result
    });
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_ne!(observer.status(), CleanupStatus::Pending);
    assert!(h.fixture.records().iter().all(|r| r["pid"] == pid));
    let exported = h
        .fixture
        .records()
        .iter()
        .any(|r| r["kind"] == "context_export");
    if bad_cut {
        assert!(!exported);
    }
    if mode == "continue_capture_duplicate" {
        assert!(matches!(
            result,
            Err(RuntimeError::Protocol("unexpected terminal context phase"))
        ));
    }
    match result {
        Ok(CaptureExit::Prepared(prepared)) => {
            assert!(exported);
            assert_eq!(observer.status(), CleanupStatus::Complete);
            assert!(prepared.verified().restored_origin().is_some());
            let (verified, _cleanup) = prepared.into_owner_parts();
            assert_eq!(verified.payload(), b"native-baseline-plus-new-delta");
            true
        }
        _ => false,
    }
}
#[test]
fn restored_export_validates_separate_baseline_delta_then_checked_cleanup() {
    assert!(scenario("continue_capture", ""));
}
#[test]
fn parent_generation_and_profile_mismatch_prevent_export() {
    for mutation in ["missing_parent", "wrong_parent", "generation", "profile"] {
        assert!(!scenario("continue_capture", mutation), "{mutation}");
    }
}
#[test]
fn malformed_duplicate_and_stalled_native_candidates_never_prepare() {
    for mode in [
        "continue_capture_challenge",
        "continue_capture_payload",
        "continue_capture_duplicate",
        "continue_capture_stall",
    ] {
        assert!(!scenario(mode, ""), "{mode}");
    }
}

#[test]
fn cancellation_and_native_owner_rejection_cannot_prepare_child() {
    for mutation in ["cancel_validation", "reject_validation"] {
        assert!(!scenario("continue_capture", mutation));
    }
}
#[test]
fn absent_export_capability_cannot_enter_capture_or_dispatch_buffered_tools() {
    refuses_capture_before_dispatch("continue_ok", false);
}
#[test]
fn historical_policy_cannot_replace_preauthorized_effective_capture_policy() {
    refuses_capture_before_dispatch("continue_capture", true);
}
fn refuses_capture_before_dispatch(mode: &str, wrong_config: bool) {
    let mut h = Harness::new(mode);
    let (_, digest) = h.grant();
    let job = h.accepted(digest);
    h.replies
        .send(ContinuationReply::Activated { job })
        .ok()
        .unwrap();
    assert!(h.pending.poll(&h.cancellation).unwrap());
    let admitted = h.pending.into_admitted().unwrap();
    let session = Session {
        id: "session".into(),
        prompt: "historical".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("new".into()),
        version: 2,
    };
    let (owner, endpoint) = CaptureOwner::channel();
    let (requests, received) = mpsc::sync_channel(2);
    let (_reply, replies) = mpsc::sync_channel(2);
    let (_messages, messages) = mpsc::channel();
    let mut selected = profile();
    if wrong_config {
        selected.configuration_digest = std::array::from_fn(|i| {
            u8::from_str_radix(&ContextDigest::of(b"config").to_hex()[i * 2..i * 2 + 2], 16)
                .unwrap()
        });
    }
    let result = admitted.run_restored_capture(
        &h.fixture.spec,
        &session,
        &h.cancellation,
        &messages,
        RuntimePersistence { requests, replies },
        FreshCaptureOptions {
            profile: selected,
            owner,
            policy: None,
            phase_timeout: Duration::from_secs(1),
        },
    );
    // Checked cleanup can report discarded buffered output before the policy error.
    // Neither policy mismatch nor cleanup failure may enter the coordinator.
    assert!(result.is_err());
    assert!(received.try_recv().is_err());
    assert!(endpoint.requests.try_recv().is_err());
    assert!(
        !h.fixture
            .records()
            .iter()
            .any(|r| r["kind"] == "tool_result")
    );
}
