use super::{
    harness::{Harness, profile},
    *,
};
use crate::coordinated::{capture::*, engine::EngineExit, *};
use foundry_contracts::{AdapterEvent, CancelMode, Outcome};

pub(super) fn cut() -> CaptureCut {
    CaptureCut::from_trusted_owner(
        CoreCutIdentity {
            session_id: "s".into(),
            attempt_id: "attempt".into(),
            request_id: "request".into(),
            expected_head: None,
        },
        CoreCutFacts {
            work_revision: 1,
            event_seq: 3,
            event_digest: [1; 32],
            input_digest: [2; 32],
            tool_digest: [3; 32],
        },
        *profile(),
        DurableCoverage {
            message_ids: vec![],
            tool_call_ids: vec![],
        },
    )
    .unwrap()
}
pub(super) fn attestation() -> OwnerValidationAttestation {
    OwnerValidationAttestation {
        validator_id: ContextId::new("offline-fixture").unwrap(),
        validator_version: ContextId::new("1").unwrap(),
        completed_turn_id: ContextId::new("turn").unwrap(),
    }
}
pub(super) fn freeze(h: &Harness) -> (CheckpointId, CaptureCorrelation) {
    h.ready();
    let (activation, _) = h.activation();
    h.activate(activation.clone());
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::Text { .. }));
    h.commit(job);
    let (terminal, event) = h.event();
    assert!(matches!(event, AdapterEvent::Settled { result, .. } if result.is_empty()));
    let CaptureRequest::FreezeAdmission { correlation } = h
        .owner
        .requests
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("freeze")
    };
    assert!(correlation.job != activation.job);
    (terminal, correlation)
}
pub(super) fn validation(
    h: &Harness,
    terminal: CheckpointId,
    frozen: CaptureCorrelation,
) -> (CaptureCorrelation, Box<CaptureValidationJob>) {
    h.owner
        .replies
        .send(CaptureReply::AdmissionFrozen {
            correlation: frozen.clone(),
        })
        .ok()
        .unwrap();
    // Freeze alone is not a persisted clean cut.
    assert!(
        h.owner
            .requests
            .recv_timeout(Duration::from_millis(30))
            .is_err()
    );
    h.commit(terminal);
    let CaptureRequest::PrepareCut { correlation } = h
        .owner
        .requests
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("cut")
    };
    assert!(correlation.job != frozen.job);
    let cut_job = correlation.job;
    h.owner
        .replies
        .send(CaptureReply::CutPrepared {
            correlation,
            cut: Box::new(cut()),
        })
        .ok()
        .unwrap();
    let CaptureRequest::ValidateCandidate { correlation, job } = h
        .owner
        .requests
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("validate")
    };
    assert!(correlation.job != cut_job);
    (correlation, job)
}

#[test]
fn prepared_requires_exact_proposal_persisted_terminal_and_joined_process() {
    let mut h = Harness::new("terminal", Duration::from_secs(2));
    let (terminal, frozen) = freeze(&h);
    let (correlation, job) = validation(&h, terminal, frozen);
    assert_eq!(job.terminal().result(), "private-final");
    assert_eq!(job.terminal().outcome(), Outcome::Success);
    assert_eq!(job.trace().entries().len(), 1);
    assert_eq!(job.trace().entries()[0].bytes(), b"fixture");
    let expected = job.binding_digest();
    let verified = job.accept_from_trusted_owner(attestation()).unwrap();
    h.owner
        .replies
        .send(CaptureReply::CandidateVerified {
            correlation,
            verified: Box::new(verified),
        })
        .ok()
        .unwrap();
    let Ok(EngineExit::Prepared(prepared)) = h.finish_owned() else {
        panic!("expected private prepared result")
    };
    assert!(prepared.verified().binding_digest() == expected);
    let (verified, _cleanup) = prepared.into_owner_parts();
    assert_eq!(verified.terminal().result(), "private-final");
    assert_eq!(verified.payload(), b"opaque-offline-fixture");
    let pid: i32 = std::fs::read_to_string(h.root.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn substituted_owner_terminal_is_rejected_before_prepared() {
    let mut h = Harness::new("terminal", Duration::from_secs(2));
    let (terminal, frozen) = freeze(&h);
    let (correlation, job) = validation(&h, terminal, frozen);
    let export = h
        .commands()
        .into_iter()
        .find(|c| c["type"] == "context_export")
        .unwrap();
    let challenge = ContextReceipt::new(export["challenge"].as_str().unwrap()).unwrap();
    let mut trace = DeliveredTrace::new();
    trace
        .reserve_prompt("s", "attempt", "fixture")
        .unwrap()
        .commit_enqueued();
    let event = ContextEvent::Candidate {
        attempt_id: correlation.attempt_id.clone(),
        capture_id: correlation.capture_id.clone(),
        challenge: challenge.clone(),
        format: profile().wire,
        coverage: ContextCoverage {
            completed_turn_id: ContextId::new("turn").unwrap(),
            inputs: vec![],
            results: vec![],
        },
        payload: ContextPayload::new(job.candidate().payload().to_vec()).unwrap(),
    };
    let substituted = CaptureValidationJob::new(
        &correlation,
        cut(),
        event,
        &challenge,
        trace,
        CaptureTerminal::new(
            foundry_contracts::Lifecycle::Exit,
            Outcome::Failure,
            "substituted",
        )
        .unwrap(),
    )
    .unwrap();
    assert!(substituted.binding_digest() != job.binding_digest());
    let verified = substituted
        .accept_from_trusted_owner(attestation())
        .unwrap();
    h.owner
        .replies
        .send(CaptureReply::CandidateVerified {
            correlation,
            verified: Box::new(verified),
        })
        .ok()
        .unwrap();
    assert!(matches!(
        h.finish_owned(),
        Err(RuntimeError::Protocol("verified capture binding mismatch"))
    ));
}

#[test]
fn post_candidate_exit_fault_or_output_cannot_prepare() {
    for mode in ["terminal_exit", "terminal_bad", "terminal_late"] {
        let mut h = Harness::new(mode, Duration::from_secs(2));
        let (terminal, frozen) = freeze(&h);
        let (correlation, job) = validation(&h, terminal, frozen);
        let verified = job.accept_from_trusted_owner(attestation()).unwrap();
        let _ = h.owner.replies.try_send(CaptureReply::CandidateVerified {
            correlation,
            verified: Box::new(verified),
        });
        assert!(h.finish_owned().is_err(), "{mode}");
    }
}

#[test]
fn queued_claim_cancel_and_owner_timeout_never_prepare() {
    for mode in ["queued", "cancel", "timeout", "wrong_job"] {
        let mut h = Harness::new("terminal", Duration::from_millis(400));
        let (terminal, mut correlation) = freeze(&h);
        match mode {
            "queued" => h
                .messages
                .send(crate::AdapterMessage {
                    message_id: "m".into(),
                    text: "late".into(),
                })
                .unwrap(),
            "cancel" => h.cancel.send(CancelMode::Hard).unwrap(),
            "wrong_job" => {
                correlation.job = CaptureJobId::fresh();
                h.owner
                    .replies
                    .send(CaptureReply::AdmissionFrozen { correlation })
                    .ok()
                    .unwrap();
            }
            _ => {}
        }
        h.commit(terminal);
        let result = h.finish_owned();
        if mode == "cancel" {
            assert!(matches!(
                result,
                Ok(EngineExit::Ordinary(crate::RunResult::Cancelled { .. }))
            ));
        } else {
            assert!(result.is_err(), "{mode}");
        }
        assert!(h.commands().iter().all(|c| c["type"] != "context_export"));
    }
}

#[test]
fn cancellation_and_deadline_during_native_owner_wait_suppress_verified_authority() {
    for cancel in [true, false] {
        let mut h = Harness::new("terminal", Duration::from_millis(400));
        let (terminal, frozen) = freeze(&h);
        let (correlation, job) = validation(&h, terminal, frozen);
        if cancel {
            h.cancel.send(CancelMode::Hard).unwrap();
        } else {
            std::thread::sleep(Duration::from_millis(450));
        }
        let verified = job.accept_from_trusted_owner(attestation()).unwrap();
        let _ = h.owner.replies.try_send(CaptureReply::CandidateVerified {
            correlation,
            verified: Box::new(verified),
        });
        let result = h.finish_owned();
        if cancel {
            assert!(matches!(
                result,
                Ok(EngineExit::Ordinary(crate::RunResult::Cancelled { .. }))
            ));
        } else {
            assert!(result.is_err());
        }
    }
}
