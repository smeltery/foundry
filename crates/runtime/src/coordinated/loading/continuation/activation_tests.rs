use super::{support::Harness, *};
use crate::{
    AdapterMessage,
    coordinated::{capture_owner::*, *},
};
use foundry_contracts::{AdapterEvent, Lifecycle, Session};
use std::{sync::mpsc, thread};

#[test]
fn restored_steering_waits_for_exact_activation_after_s2_without_start() {
    for wrong in [false, true] {
        let mut h = Harness::new("continue_capture_steer");
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
        let (reply, replies) = mpsc::sync_channel(2);
        let (sent, messages) = mpsc::channel();
        sent.send(AdapterMessage {
            message_id: "steer".into(),
            text: "new instruction".into(),
        })
        .unwrap();
        let cancellation = h.cancellation;
        let result = thread::scope(|scope| {
            let spec = &h.fixture.spec;
            let runtime = scope.spawn(move || {
                admitted.run_restored_capture(
                    spec,
                    &session,
                    &cancellation,
                    &messages,
                    RuntimePersistence { requests, replies },
                    FreshCaptureOptions {
                        profile: super::capture_tests::profile(),
                        owner,
                        policy: None,
                        phase_timeout: Duration::from_secs(2),
                    },
                )
            });
            let CaptureRequest::ActivateAdmission {
                mut correlation,
                message_ack,
            } = endpoint
                .requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            else {
                panic!()
            };
            assert!(message_ack);
            assert_eq!(correlation.attempt_id.as_str(), "new");
            assert!(matches!(
                received.recv_timeout(Duration::from_millis(40)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            assert!(h.fixture.records().iter().all(|r| r["kind"] != "message"));
            if wrong {
                correlation.job = CaptureJobId::fresh();
            }
            endpoint
                .replies
                .send(CaptureReply::AdmissionActivated { correlation })
                .ok()
                .unwrap();
            if !wrong {
                let PersistenceRequest::Event {
                    checkpoint,
                    event: AdapterEvent::MessageAccepted { message_id, .. },
                } = received.recv_timeout(Duration::from_secs(2)).unwrap()
                else {
                    panic!()
                };
                assert_eq!(message_id, "steer");
                reply
                    .send(PersistenceReply::EventCommitted { checkpoint })
                    .unwrap();
                h.cancel.send(CancelMode::Hard).unwrap();
            }
            runtime.join().unwrap()
        });
        if wrong {
            assert!(result.is_err());
        } else {
            assert!(matches!(result, Ok(CaptureExit::Cancelled(_))));
        }
        let records = h.fixture.records();
        assert_eq!(
            records
                .iter()
                .filter(|r| r["kind"] == "continue_context_v2")
                .count(),
            1
        );
        assert!(records.iter().all(|r| r["kind"] != "start"));
        assert_eq!(
            records.iter().filter(|r| r["kind"] == "message").count(),
            usize::from(!wrong)
        );
    }
}
