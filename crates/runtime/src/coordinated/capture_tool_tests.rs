use super::{harness::Harness, *};
use crate::coordinated::{PersistenceReply, PersistenceRequest, ToolDisposition, ToolEvidence};
use foundry_contracts::AdapterEvent;

pub(super) fn finish_request(h: &Harness) -> (crate::coordinated::CheckpointId, ToolEvidence) {
    h.ready();
    let (correlation, _) = h.activation();
    h.activate(correlation);
    let (job, event) = h.event();
    assert!(matches!(event, AdapterEvent::ToolCall { .. }));
    h.commit(job);
    let PersistenceRequest::ClaimTool { checkpoint, .. } =
        h.requests.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("claim required")
    };
    h.replies
        .send(PersistenceReply::ToolClaimed { checkpoint })
        .unwrap();
    let PersistenceRequest::FinishToolWithEvidence {
        checkpoint,
        evidence,
    } = h.requests.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("explicit evidence request required for every capture outcome")
    };
    (checkpoint, evidence)
}

#[test]
fn capture_requests_evidence_for_shared_failed_spawn_and_uncertain_results() {
    for mode in ["ok", "error", "uncertain", "missing", "bad"] {
        let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some(mode));
        let (checkpoint, evidence) = finish_request(&h);
        let (disposition, output_digest) = match (mode, evidence) {
            ("ok", ToolEvidence::SharedReply(reply)) => {
                let clone = reply.clone();
                assert!(std::ptr::eq(reply.reply(), clone.reply()));
                (ToolDisposition::Success, Some([9; 32]))
            }
            ("error", ToolEvidence::SharedReply(_)) => {
                (ToolDisposition::NoEffectFailure, Some([9; 32]))
            }
            // A real unknown-effect reply has a digest; standalone uncertainty does not.
            ("uncertain", ToolEvidence::SharedReply(_)) => {
                (ToolDisposition::Uncertain, Some([9; 32]))
            }
            ("missing", ToolEvidence::Uncertain) => (ToolDisposition::Uncertain, None),
            ("bad", ToolEvidence::Uncertain) => (ToolDisposition::Uncertain, None),
            _ => panic!("unexpected capture evidence shape for {mode}"),
        };
        h.replies
            .send(PersistenceReply::ToolFinishedEvidence {
                checkpoint,
                disposition,
                output_digest,
            })
            .unwrap();
        if disposition == ToolDisposition::Uncertain {
            assert!(matches!(
                h.finish(),
                Err(RuntimeError::Protocol("tool effects are uncertain"))
            ));
            assert!(
                h.commands()
                    .iter()
                    .all(|command| command["type"] != "tool_result")
            );
        } else {
            let (job, event) = h.event();
            assert!(matches!(event, AdapterEvent::Text { text, .. } if text == "tool-result"));
            h.commit(job);
            assert_eq!(h.commands()[2]["type"], "tool_result");
            h.cancel.send(foundry_contracts::CancelMode::Hard).unwrap();
            assert!(matches!(h.finish(), Ok(crate::RunResult::Cancelled { .. })));
        }
    }
}

#[test]
fn capture_rejects_legacy_ack_or_missing_shared_reply_digest() {
    for legacy in [true, false] {
        let mut h = Harness::with_helper("tool", Duration::from_secs(2), Some("ok"));
        let (checkpoint, _) = finish_request(&h);
        let reply = if legacy {
            PersistenceReply::ToolFinished {
                checkpoint,
                disposition: ToolDisposition::Success,
            }
        } else {
            PersistenceReply::ToolFinishedEvidence {
                checkpoint,
                disposition: ToolDisposition::Success,
                output_digest: None,
            }
        };
        h.replies.send(reply).unwrap();
        assert!(matches!(
            h.finish(),
            Err(RuntimeError::Protocol("tool receipt evidence mismatch"))
        ));
        assert_eq!(h.commands().len(), 2);
    }
}

#[test]
fn full_profile_is_retained_and_configuration_digest_is_not_rehashed() {
    let (owner, _endpoint) = CaptureOwner::channel();
    let mut profile = super::harness::profile();
    profile.stored.binding_version = "independent-version".into();
    let state = Startup::new(
        StartupPlan {
            profile,
            timeout: Duration::from_secs(1),
        },
        owner,
        "attempt",
    )
    .unwrap();
    assert_eq!(
        state.plan.profile.stored.binding_version,
        "independent-version"
    );
    assert_eq!(state.plan.profile.wire.producer_build.as_str(), "fixture");
    assert_eq!(state.configuration_digest.to_hex(), "07".repeat(32));
}
