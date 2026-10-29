use super::{harness::Harness, *};
use crate::coordinated::{engine::EngineExit, *};
use foundry_contracts::{AdapterEvent, CancelMode};

fn claimed(h: &Harness) -> CheckpointId {
    h.ready();
    let (correlation, _) = h.activation();
    h.activate(correlation);
    let (checkpoint, event) = h.event();
    assert!(matches!(event, AdapterEvent::ToolCall { .. }));
    h.commit(checkpoint);
    let PersistenceRequest::ClaimTool { checkpoint, .. } =
        h.requests.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!()
    };
    checkpoint
}
fn block(h: &Harness, checkpoint: CheckpointId, disposition: PermissionDisposition) {
    h.replies
        .send(PersistenceReply::ToolBlocked {
            checkpoint,
            disposition,
        })
        .unwrap();
}

#[test]
fn checked_stop_is_bound_to_exact_owner_and_never_prepares_capture() {
    let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some("ok"));
    let checkpoint = claimed(&h);
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    let EngineExit::ActionRequired(stop) = h.finish_owned().unwrap() else {
        panic!()
    };
    assert!(h.owner.matches_action_required(&stop));
    let (_owner, other) = CaptureOwner::channel();
    assert!(!other.matches_action_required(&stop));
    assert_eq!(stop.session_id(), "s");
    assert_eq!(stop.attempt_id(), "attempt");
    assert_eq!(h.commands().len(), 2);
    assert!(h.owner.requests.try_recv().is_err());
    assert!(h.requests.try_recv().is_err());
    let _cleanup = stop.into_cleanup();
}

#[test]
fn permission_stop_uses_run_deadline_not_elapsed_startup_timer() {
    let mut h = Harness::with_helper("tool", Duration::from_secs(1), Some("ok"));
    let checkpoint = claimed(&h);
    std::thread::sleep(Duration::from_millis(1100));
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    assert!(matches!(
        h.finish_owned(),
        Ok(EngineExit::ActionRequired(_))
    ));
}

#[test]
fn cancellation_and_unsupported_do_not_mint_cleanup_stop() {
    for cancel in [false, true] {
        let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some("ok"));
        let checkpoint = claimed(&h);
        if cancel {
            h.cancel.send(CancelMode::Hard).unwrap();
        }
        block(
            &h,
            checkpoint,
            if cancel {
                PermissionDisposition::ActionRequired
            } else {
                PermissionDisposition::Unsupported
            },
        );
        let result = h.finish_owned();
        if cancel {
            assert!(matches!(
                result,
                Ok(EngineExit::Ordinary(crate::RunResult::Cancelled { .. }))
            ));
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Protocol("permission_unsupported"))
            ));
        }
    }
}

#[test]
fn unsolicited_owner_reply_prevents_permission_handoff() {
    let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some("ok"));
    let checkpoint = claimed(&h);
    h.owner
        .replies
        .send(CaptureReply::AdmissionActivated {
            correlation: CaptureCorrelation {
                job: CaptureJobId::fresh(),
                attempt_id: ContextId::new("attempt").unwrap(),
                capture_id: ContextId::new("wrong").unwrap(),
            },
        })
        .unwrap();
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    assert!(h.finish_owned().is_err());
}

#[test]
fn duplicate_permission_receipt_prevents_cleanup_handoff() {
    let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some("ok"));
    let checkpoint = claimed(&h);
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    assert!(h.finish_owned().is_err());
}

#[test]
fn active_stop_rejects_disconnected_owner_or_unfinished_terminal() {
    for terminal in [false, true] {
        let (owner, endpoint) = CaptureOwner::channel();
        let mut state = Startup::new(
            StartupPlan {
                profile: harness::profile(),
                timeout: Duration::from_secs(1),
            },
            owner,
            "attempt",
        )
        .unwrap();
        state.enter(Phase::Active);
        if terminal {
            state
                .trace
                .reserve_prompt("s", "attempt", "prompt")
                .unwrap()
                .commit_enqueued();
            state
                .begin_terminal(foundry_contracts::Outcome::Success, "done")
                .unwrap();
        } else {
            drop(endpoint);
        }
        assert!(state.check_permission_stop().is_err());
    }
}

#[test]
fn partial_transport_frame_during_pending_prevents_checked_stop() {
    let mut h = Harness::with_helper("tool_partial", Duration::from_secs(2), Some("ok"));
    let checkpoint = claimed(&h);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !h.root.join("partial").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Let the pipe worker enter its read of the partial frame before shutdown.
    std::thread::sleep(Duration::from_millis(30));
    block(&h, checkpoint, PermissionDisposition::ActionRequired);
    assert!(matches!(
        h.finish_owned(),
        Err(RuntimeError::Protocol(
            "capture transport did not close cleanly"
        ))
    ));
    assert_eq!(h.commands().len(), 2);
}
