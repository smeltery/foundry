use super::{harness::Harness, *};
use crate::coordinated::engine::EngineExit;
use foundry_contracts::CancelMode;

#[test]
fn cancellation_before_activation_is_bound_without_native_evidence() {
    for ready in [false, true] {
        let mut h = Harness::new("normal", Duration::from_secs(2));
        if ready {
            h.ready();
            let _ = h.activation();
        } else {
            let _ = h.event();
        }
        h.cancel.send(CancelMode::Hard).unwrap();
        let EngineExit::Cancelled(stop) = h.finish_owned().unwrap() else {
            panic!()
        };
        assert!(h.owner.matches_cancelled(&stop));
        let (_, other) = CaptureOwner::channel();
        assert!(!other.matches_cancelled(&stop));
        assert_eq!(stop.session_id(), "s");
        assert_eq!(stop.attempt_id(), "attempt");
        assert_eq!(stop.mode(), CancelMode::Hard);
        assert!(stop.forced());
        assert!(h.commands().iter().all(|c| c["type"] != "context_export"));
    }
}

#[test]
fn active_graceful_cancel_has_cleanup_without_publication() {
    let mut h = Harness::new("normal", Duration::from_secs(2));
    h.ready();
    let (correlation, _) = h.activation();
    h.activate(correlation);
    h.cancel.send(CancelMode::Graceful).unwrap();
    let EngineExit::Cancelled(stop) = h.finish_owned().unwrap() else {
        panic!()
    };
    assert!(h.owner.matches_cancelled(&stop));
    assert_eq!(stop.mode(), CancelMode::Graceful);
    assert!(h.commands().iter().all(|c| c["type"] != "context_export"));
}

#[test]
fn generated_capture_job_accessor_preserves_identity() {
    let job = CaptureJobId::fresh();
    assert_eq!(job.as_uuid(), &job.0);
    assert_eq!(job.as_uuid().get_version_num(), 4);
}

#[test]
fn retained_matchers_reject_other_channels_with_identical_labels() {
    let make = || {
        let mut h = Harness::new("normal", Duration::from_secs(2));
        let _ = h.event();
        let matcher = h.owner.cancellation_matcher();
        h.cancel.send(CancelMode::Hard).unwrap();
        let EngineExit::Cancelled(stop) = h.finish_owned().unwrap() else {
            panic!()
        };
        assert!(matcher.matches(&stop));
        drop(h);
        (matcher, stop)
    };
    let (first, first_stop) = make();
    let (second, second_stop) = make();
    assert_eq!(first_stop.session_id(), second_stop.session_id());
    assert_eq!(first_stop.attempt_id(), second_stop.attempt_id());
    assert!(first.matches(&first_stop));
    assert!(second.matches(&second_stop));
    assert!(!first.matches(&second_stop));
    assert!(!second.matches(&first_stop));
}
