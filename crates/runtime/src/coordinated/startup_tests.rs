use super::{harness::Harness, *};
use crate::{AdapterMessage, RunResult};
use foundry_contracts::{AdapterEvent, CancelMode};
use std::thread;

#[test]
fn startup_waits_for_persisted_ready_and_activation_before_messages() {
    let mut h = Harness::new("settle", Duration::from_secs(2));
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::Ready { .. }));
    thread::sleep(Duration::from_millis(40));
    assert!(h.commands().is_empty());
    h.commit(job);
    let (correlation, message_ack) = h.activation();
    assert!(message_ack);
    let commands = h.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["type"], "context_capture_begin");
    assert_eq!(commands[0]["capture_id"], correlation.capture_id.as_str());
    assert_eq!(commands[1]["type"], "start");
    h.messages
        .send(AdapterMessage {
            message_id: "m".into(),
            text: "literal".into(),
        })
        .unwrap();
    assert!(h.requests.recv_timeout(Duration::from_millis(40)).is_err());
    assert_eq!(h.commands().len(), 2);
    h.activate(correlation);
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::Text { .. }));
    h.commit(job);
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::MessageAccepted { .. }));
    h.commit(job);
    let (_, event) = h.event();
    assert!(matches!(event, AdapterEvent::Settled { .. }));
    h.cancel.send(CancelMode::Hard).unwrap();
    assert!(matches!(h.finish(), Ok(RunResult::Cancelled { .. })));
}

#[test]
fn tool_requests_are_backpressured_until_activation() {
    let mut h = Harness::new("tool", Duration::from_secs(2));
    h.ready();
    let (correlation, _) = h.activation();
    assert!(h.requests.recv_timeout(Duration::from_millis(40)).is_err());
    assert_eq!(h.commands().len(), 2);
    h.activate(correlation);
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::ToolCall { .. }));
    assert_eq!(h.commands().len(), 2);
    h.commit(job);
    let (_, event) = h.event();
    assert!(matches!(event, AdapterEvent::Text { text, .. } if text == "tool-result"));
    assert_eq!(h.commands()[2]["result"]["status"], "denied");
    h.cancel.send(CancelMode::Hard).unwrap();
    assert!(matches!(h.finish(), Ok(RunResult::Cancelled { .. })));
}

#[test]
fn missing_capabilities_wrong_bindings_and_premature_output_never_start() {
    for mode in [
        "missing",
        "missing_export",
        "capture_id",
        "attempt_id",
        "format_digest",
        "configuration_digest",
        "challenge",
        "premature",
        "restored",
        "timeout",
    ] {
        let mut h = Harness::new(mode, Duration::from_secs(1));
        if !mode.starts_with("missing") {
            h.ready();
        }
        assert!(h.finish().is_err(), "{mode}");
        assert!(
            h.commands()
                .iter()
                .all(|command| command["type"] != "start"),
            "{mode}"
        );
        assert!(h.owner.requests.try_recv().is_err());
    }
}

#[test]
fn cancellation_during_activation_wins_over_queued_owner_permission() {
    for mode in [CancelMode::Hard, CancelMode::Graceful] {
        let mut h = Harness::new("normal", Duration::from_secs(2));
        h.ready();
        let (correlation, _) = h.activation();
        h.messages
            .send(AdapterMessage {
                message_id: "m".into(),
                text: "literal".into(),
            })
            .unwrap();
        h.cancel.send(mode).unwrap();
        let _ = h
            .owner
            .replies
            .try_send(CaptureReply::AdmissionActivated { correlation });
        assert!(matches!(h.finish(), Ok(RunResult::Cancelled { .. })));
        assert!(h.requests.try_recv().is_err());
        assert!(
            h.commands()
                .iter()
                .all(|command| command["type"] != "message")
        );
    }
}

#[test]
fn activation_timeout_wrong_correlation_and_rejection_cannot_admit_output() {
    for mode in ["timeout", "attempt", "capture", "job", "rejected"] {
        let mut h = Harness::new("normal", Duration::from_secs(1));
        h.ready();
        let (mut correlation, _) = h.activation();
        match mode {
            "attempt" => correlation.attempt_id = ContextId::new("other").unwrap(),
            "capture" => correlation.capture_id = ContextId::new("other").unwrap(),
            "job" => correlation.job = CaptureJobId(uuid::Uuid::new_v4()),
            _ => {}
        }
        if mode == "rejected" {
            h.owner
                .replies
                .send(CaptureReply::Rejected {
                    correlation,
                    code: CaptureRejection::OwnershipLost,
                })
                .ok()
                .unwrap();
        } else if mode != "timeout" {
            h.activate(correlation);
        }
        assert!(h.finish().is_err());
        assert!(h.requests.try_recv().is_err());
    }
}

#[test]
fn unsupported_steering_remains_disabled_after_activation() {
    let mut h = Harness::new("no_messages", Duration::from_secs(2));
    h.ready();
    let (correlation, enabled) = h.activation();
    assert!(!enabled);
    h.messages
        .send(AdapterMessage {
            message_id: "m".into(),
            text: "literal".into(),
        })
        .unwrap();
    h.activate(correlation);
    let (job, _) = h.event();
    h.commit(job);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(h.commands().len(), 2);
    h.cancel.send(CancelMode::Hard).unwrap();
    assert!(matches!(h.finish(), Ok(RunResult::Cancelled { .. })));
}

fn owner_state() -> (Startup, CaptureOwnerEndpoint) {
    let (owner, endpoint) = CaptureOwner::channel();
    let state = Startup::new(
        StartupPlan {
            profile: super::harness::profile(),
            timeout: Duration::from_millis(50),
        },
        owner,
        "attempt",
    )
    .unwrap();
    (state, endpoint)
}

#[test]
fn full_owner_queue_retains_exact_job_and_times_out_without_blocking() {
    let (mut state, endpoint) = owner_state();
    state.phase = Phase::ActivationPending;
    for _ in 0..CAPTURE_OWNER_CAPACITY {
        state
            .owner
            .requests
            .try_send(CaptureRequest::ActivateAdmission {
                correlation: state.correlation.clone(),
                message_ack: false,
            })
            .ok()
            .unwrap();
    }
    state.pending = Some(CaptureRequest::ActivateAdmission {
        correlation: state.correlation.clone(),
        message_ack: true,
    });
    state.submit_activation().unwrap();
    assert!(
        matches!(&state.pending, Some(CaptureRequest::ActivateAdmission { correlation, message_ack: true }) if correlation == &state.correlation)
    );
    endpoint.requests.try_recv().unwrap();
    state.submit_activation().unwrap();
    assert!(state.pending.is_none());
    endpoint.requests.try_recv().unwrap();
    let CaptureRequest::ActivateAdmission {
        correlation,
        message_ack,
    } = endpoint.requests.try_recv().unwrap()
    else {
        panic!("expected activation")
    };
    assert!(correlation == state.correlation && message_ack);
    state.since = Instant::now() - Duration::from_secs(1);
    assert!(state.poll_owner().is_err());
}

#[test]
fn duplicate_unsolicited_and_disconnected_owner_replies_fail_closed() {
    for phase in [Phase::AwaitReady, Phase::PersistingReady, Phase::Active] {
        let (mut state, endpoint) = owner_state();
        state.phase = phase;
        endpoint
            .replies
            .try_send(CaptureReply::AdmissionActivated {
                correlation: state.correlation.clone(),
            })
            .ok()
            .unwrap();
        assert!(state.poll_owner().is_err());
    }
    let (mut state, endpoint) = owner_state();
    state.phase = Phase::ActivationPending;
    for _ in 0..2 {
        endpoint
            .replies
            .try_send(CaptureReply::AdmissionActivated {
                correlation: state.correlation.clone(),
            })
            .ok()
            .unwrap();
    }
    assert!(state.poll_owner().is_err());
    assert!(!state.active());
    let (mut state, endpoint) = owner_state();
    drop(endpoint);
    assert!(state.poll_owner().is_err());
    state.pending = Some(CaptureRequest::ActivateAdmission {
        correlation: state.correlation.clone(),
        message_ack: false,
    });
    assert!(state.submit_activation().is_err());
}

#[test]
fn startup_timeout_must_be_positive_and_bounded() {
    for timeout in [Duration::ZERO, Duration::from_secs(61)] {
        let (owner, _endpoint) = CaptureOwner::channel();
        assert!(
            Startup::new(
                StartupPlan {
                    profile: super::harness::profile(),
                    timeout,
                },
                owner,
                "attempt"
            )
            .is_err()
        );
    }
}

#[test]
fn slow_fixture_readiness_exhausts_phase_budget_before_persistence() {
    let mut h = Harness::new("delayed_ready", Duration::from_millis(80));
    assert!(matches!(
        h.finish(),
        Err(RuntimeError::Protocol("capture startup deadline exceeded"))
    ));
    assert!(h.requests.try_recv().is_err());
    assert!(h.commands().is_empty());
    assert!(h.owner.requests.try_recv().is_err());
}
