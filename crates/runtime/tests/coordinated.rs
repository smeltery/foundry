#![cfg(unix)]
#[path = "coordinated/support.rs"]
mod support;
use foundry_contracts::{AdapterEvent, CancelMode};
use foundry_runtime::{RunResult, RuntimeError, coordinated::*};
use foundry_tools::host;
use std::time::Duration;
use support::*;

#[test]
fn claim_precedes_spawn_and_receipt_precedes_result_and_settlement() {
    let fixture = Fixture::new("ok");
    let run = fixture.start("normal", true);
    let checkpoint = run.claim();
    std::thread::sleep(Duration::from_millis(50));
    assert!(!fixture.root.join("helper-started").exists());
    run.claimed(checkpoint);
    let PersistenceRequest::FinishTool {
        checkpoint: finish,
        evidence: ToolEvidence::Replied(reply),
    } = run.next()
    else {
        panic!("expected cleaned reply")
    };
    assert_eq!(finish, checkpoint);
    assert!(matches!(reply.result, host::Outcome::Ok { .. }));
    assert_eq!(std::fs::read(fixture.root.join("effects")).unwrap(), b"x");
    assert!(!fixture.root.join("adapter-result").exists());
    run.replies
        .send(PersistenceReply::ToolFinished {
            checkpoint,
            disposition: ToolDisposition::Success,
        })
        .unwrap();
    let PersistenceRequest::Event {
        checkpoint,
        event: AdapterEvent::Settled { .. },
    } = run.next()
    else {
        panic!("expected terminal event")
    };
    assert!(run.results.recv_timeout(Duration::from_millis(50)).is_err());
    run.replies
        .send(PersistenceReply::EventCommitted { checkpoint })
        .unwrap();
    assert!(matches!(run.finish(), Ok(RunResult::Completed { .. })));
}

#[test]
fn cancellation_revokes_pending_claim_and_late_permission() {
    let fixture = Fixture::new("ok");
    let run = fixture.start("normal", true);
    let checkpoint = run.claim();
    run.cancel.send(CancelMode::Hard).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let _ = run
        .replies
        .try_send(PersistenceReply::ToolClaimed { checkpoint });
    assert!(matches!(run.finish(), Ok(RunResult::Cancelled { .. })));
    assert!(!fixture.root.join("helper-started").exists());
}

#[test]
fn rejected_receipt_does_not_release_adapter_result_or_repeat_helper() {
    let fixture = Fixture::new("ok");
    let run = fixture.start("normal", true);
    let checkpoint = run.claim();
    run.claimed(checkpoint);
    assert!(matches!(run.next(), PersistenceRequest::FinishTool { .. }));
    run.replies
        .send(PersistenceReply::Rejected {
            checkpoint,
            code: PersistenceFailure::Storage,
        })
        .unwrap();
    assert!(run.finish().is_err());
    assert!(!fixture.root.join("adapter-result").exists());
    assert_eq!(std::fs::read(fixture.root.join("effects")).unwrap(), b"x");
}

#[test]
fn default_denial_and_input_grant_injection_never_spawn_helpers() {
    for (adapter, enabled) in [("normal", false), ("inject", true)] {
        let fixture = Fixture::new("ok");
        let run = fixture.start(adapter, enabled);
        run.event();
        run.event();
        assert!(matches!(run.event(), AdapterEvent::Settled { .. }));
        assert!(matches!(run.finish(), Ok(RunResult::Completed { .. })));
        assert!(!fixture.root.join("helper-started").exists());
        assert!(
            std::fs::read_to_string(fixture.root.join("adapter-result"))
                .unwrap()
                .contains("denied")
        );
    }
}

#[test]
fn premature_settlement_and_parallel_tool_calls_fail() {
    for mode in ["premature", "parallel"] {
        let fixture = Fixture::new("ok");
        let run = fixture.start(mode, true);
        run.event();
        run.event();
        assert!(matches!(run.finish(), Err(RuntimeError::Protocol(_))));
        assert!(!fixture.root.join("helper-started").exists());
    }
}

#[test]
fn no_effect_error_is_delivered_only_after_its_receipt() {
    let fixture = Fixture::new("error");
    let run = fixture.start("normal", true);
    let checkpoint = run.claim();
    run.claimed(checkpoint);
    let PersistenceRequest::FinishTool {
        evidence: ToolEvidence::Replied(reply),
        ..
    } = run.next()
    else {
        panic!("expected reply")
    };
    assert!(matches!(
        reply.result,
        host::Outcome::Error {
            effect: host::Effect::None,
            ..
        }
    ));
    assert!(!fixture.root.join("adapter-result").exists());
    run.replies
        .send(PersistenceReply::ToolFinished {
            checkpoint,
            disposition: ToolDisposition::NoEffectFailure,
        })
        .unwrap();
    run.event();
    assert!(run.finish().is_ok());
    assert!(!fixture.root.join("effects").exists());
}

#[test]
fn uncertain_or_invalid_helper_reply_never_becomes_success() {
    for mode in ["uncertain", "bad"] {
        let fixture = Fixture::new(mode);
        let run = fixture.start("normal", true);
        let checkpoint = run.claim();
        run.claimed(checkpoint);
        let PersistenceRequest::FinishTool { evidence, .. } = run.next() else {
            panic!("expected uncertain finish")
        };
        assert!(matches!(
            evidence,
            ToolEvidence::Uncertain
                | ToolEvidence::Replied(host::Reply {
                    result: host::Outcome::Error {
                        effect: host::Effect::Unknown,
                        ..
                    },
                    ..
                })
        ));
        run.replies
            .send(PersistenceReply::ToolFinished {
                checkpoint,
                disposition: ToolDisposition::Uncertain,
            })
            .unwrap();
        assert!(run.finish().is_err());
        assert!(!fixture.root.join("adapter-result").exists());
    }
}

#[test]
fn helper_timeout_progresses_while_event_persistence_is_blocked() {
    let fixture = Fixture::new("hang");
    let run = fixture.start("text", true);
    let checkpoint = run.claim();
    run.claimed(checkpoint);
    let PersistenceRequest::Event {
        checkpoint: event_checkpoint,
        event: AdapterEvent::Text { .. },
    } = run.next()
    else {
        panic!("expected text")
    };
    let PersistenceRequest::FinishTool {
        evidence: ToolEvidence::Uncertain,
        ..
    } = run.next()
    else {
        panic!("timeout must finish despite blocked event")
    };
    let pid = std::fs::read_to_string(fixture.root.join("helper-started"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    run.replies
        .send(PersistenceReply::EventCommitted {
            checkpoint: event_checkpoint,
        })
        .unwrap();
    run.replies
        .send(PersistenceReply::ToolFinished {
            checkpoint,
            disposition: ToolDisposition::Uncertain,
        })
        .unwrap();
    assert!(run.finish().is_err());
    assert!(!fixture.root.join("adapter-result").exists());
}

#[test]
fn persisted_adapter_failure_is_sanitized() {
    let fixture = Fixture::new("ok");
    let run = fixture.start("failed", false);
    run.event();
    let event = run.event();
    assert!(matches!(event, AdapterEvent::Failed { .. }));
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(!encoded.contains("private"));
    assert!(matches!(run.finish(), Err(RuntimeError::AdapterFailed)));
}

#[test]
fn saturated_persistence_queue_does_not_block_hard_cancellation() {
    let fixture = Fixture::new("ok");
    let run = fixture.start_with_capacity("normal", true, 0);
    std::thread::sleep(Duration::from_millis(50));
    run.cancel.send(CancelMode::Hard).unwrap();
    assert!(matches!(run.finish(), Ok(RunResult::Cancelled { .. })));
    assert!(!fixture.root.join("helper-started").exists());
}

#[test]
fn cancellation_after_helper_cleanup_before_receipt_suppresses_result() {
    let fixture = Fixture::new("ok");
    let run = fixture.start("normal", true);
    let checkpoint = run.claim();
    run.claimed(checkpoint);
    assert!(matches!(run.next(), PersistenceRequest::FinishTool { .. }));
    run.cancel.send(CancelMode::Hard).unwrap();
    let _ = run.replies.try_send(PersistenceReply::ToolFinished {
        checkpoint,
        disposition: ToolDisposition::Success,
    });
    assert!(matches!(run.finish(), Ok(RunResult::Cancelled { .. })));
    assert!(!fixture.root.join("adapter-result").exists());
    assert_eq!(std::fs::read(fixture.root.join("effects")).unwrap(), b"x");
}

#[test]
fn wrong_checkpoint_and_wrong_disposition_abort_without_result() {
    for wrong_checkpoint in [true, false] {
        let fixture = Fixture::new("ok");
        let run = fixture.start("normal", true);
        let PersistenceRequest::Event {
            checkpoint: ready, ..
        } = run.next()
        else {
            panic!("ready")
        };
        run.replies
            .send(PersistenceReply::EventCommitted { checkpoint: ready })
            .unwrap();
        run.event();
        let PersistenceRequest::ClaimTool { checkpoint, .. } = run.next() else {
            panic!("claim")
        };
        if wrong_checkpoint {
            run.claimed(ready);
        } else {
            run.claimed(checkpoint);
            assert!(matches!(run.next(), PersistenceRequest::FinishTool { .. }));
            run.replies
                .send(PersistenceReply::ToolFinished {
                    checkpoint,
                    disposition: ToolDisposition::NoEffectFailure,
                })
                .unwrap();
        }
        assert!(matches!(run.finish(), Err(RuntimeError::Protocol(_))));
        assert!(!fixture.root.join("adapter-result").exists());
    }
}

#[path = "coordinated/permissions.rs"]
mod permissions;
