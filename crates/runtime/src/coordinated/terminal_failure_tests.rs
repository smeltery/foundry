use super::{
    harness::Harness,
    terminal_tests::{attestation, cut, freeze, validation},
    *,
};
use crate::coordinated::{capture::*, *};

#[test]
fn wrong_freeze_identity_phase_or_duplicate_cannot_reach_cut() {
    for mode in ["attempt", "capture", "phase", "duplicate"] {
        let mut h = Harness::new("terminal", Duration::from_secs(2));
        let (terminal, mut correlation) = freeze(&h);
        h.commit(terminal);
        match mode {
            "attempt" => correlation.attempt_id = ContextId::new("foreign").unwrap(),
            "capture" => correlation.capture_id = ContextId::new("foreign").unwrap(),
            _ => {}
        }
        let reply = if mode == "phase" {
            CaptureReply::AdmissionActivated {
                correlation: correlation.clone(),
            }
        } else {
            CaptureReply::AdmissionFrozen {
                correlation: correlation.clone(),
            }
        };
        h.owner.replies.send(reply).ok().unwrap();
        if mode == "duplicate" {
            let _ = h
                .owner
                .replies
                .try_send(CaptureReply::AdmissionFrozen { correlation });
        }
        assert!(h.finish_owned().is_err(), "{mode}");
        assert!(h.commands().iter().all(|c| c["type"] != "context_export"));
    }
}

#[test]
fn cut_must_preserve_full_profile_and_cannot_invent_a_restored_baseline() {
    for mode in [
        "stored_binding",
        "stored_version",
        "configuration",
        "session",
        "restored",
    ] {
        let mut h = Harness::new("terminal", Duration::from_secs(2));
        let (terminal, correlation) = freeze(&h);
        h.owner
            .replies
            .send(CaptureReply::AdmissionFrozen { correlation })
            .ok()
            .unwrap();
        h.commit(terminal);
        let CaptureRequest::PrepareCut { correlation } = h
            .owner
            .requests
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
        else {
            panic!("cut")
        };
        let (mut identity, facts, mut profile, coverage) = cut().into_owner_parts();
        match mode {
            "stored_binding" => profile.stored.binding_id = "other".into(),
            "stored_version" => profile.stored.format_version += 1,
            "configuration" => profile.configuration_digest[0] += 1,
            "session" => identity.session_id = "other".into(),
            _ => {
                identity.expected_head = Some(HeadIdentity {
                    checkpoint_id: uuid::Uuid::new_v4().to_string(),
                    generation: 1,
                })
            }
        }
        let cut = CaptureCut::from_trusted_owner(identity, facts, profile, coverage).unwrap();
        h.owner
            .replies
            .send(CaptureReply::CutPrepared {
                correlation,
                cut: Box::new(cut),
            })
            .ok()
            .unwrap();
        assert!(h.finish_owned().is_err(), "{mode}");
        assert!(h.commands().iter().all(|c| c["type"] != "context_export"));
    }
}

#[test]
fn wrong_persistence_receipt_and_extra_validation_reply_fail_closed() {
    for mode in ["persistence", "owner"] {
        let mut h = Harness::new("terminal", Duration::from_secs(2));
        let (terminal, frozen) = freeze(&h);
        let (correlation, job) = validation(&h, terminal, frozen);
        let verified = job.accept_from_trusted_owner(attestation()).unwrap();
        if mode == "persistence" {
            h.replies
                .send(PersistenceReply::EventCommitted {
                    checkpoint: terminal,
                })
                .unwrap();
        } else {
            h.owner
                .replies
                .send(CaptureReply::AdmissionFrozen {
                    correlation: correlation.clone(),
                })
                .ok()
                .unwrap();
        }
        let _ = h.owner.replies.try_send(CaptureReply::CandidateVerified {
            correlation,
            verified: Box::new(verified),
        });
        assert!(h.finish_owned().is_err(), "{mode}");
    }
}
