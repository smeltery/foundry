use super::tests::{Fixture, plan};
use super::*;
use foundry_contracts::context_observation::{
    ObservationBinding, ObservationFailure, ObservationFrame,
};
use std::{sync::mpsc, thread};

fn binding(loaded: &LoadedProcess) -> ObservationBinding {
    ObservationBinding {
        scope: loaded.scope().clone(),
        job_id: ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        begin_binding: loaded.begin_binding(),
        profile_digest: ContextDigest::of(b"trusted-profile"),
    }
}
fn await_response(
    loaded: &mut LoadedProcess,
    cancellation: &Receiver<CancelMode>,
) -> Result<ObservationFrame, LoadingError> {
    loop {
        if let Some(frame) = loaded.poll_observation(cancellation)? {
            return Ok(frame);
        }
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn same_process_observation_is_private_correlated_and_not_native_approval() {
    let plan = plan();
    let fixture = Fixture::new("observe_ok", &plan);
    let (_owner, cancellation) = mpsc::channel();
    let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
    let pid = loaded.process_id();
    let deadline = loaded.deadline;
    let expected = binding(&loaded);
    loaded
        .begin_observation(expected.clone(), &cancellation)
        .unwrap();
    let frame = await_response(&mut loaded, &cancellation).unwrap();
    assert!(frame.binding() == &expected);
    assert_eq!(
        frame.payload().unwrap().unwrap().as_bytes(),
        b"detached-native-observation-fixture"
    );
    assert!(loaded.observed_instance() == frame.instance_id());
    assert_eq!(loaded.deadline, deadline);
    assert_eq!(loaded.process_id(), pid);
    assert_eq!(fixture.records().len(), 3);
    assert!(fixture.records().iter().all(|record| record["pid"] == pid));
    assert_eq!(fixture.records()[2]["kind"], "context_observe_v1");
    loaded.poll(&cancellation).unwrap();
    loaded.close().unwrap();
}
#[test]
fn every_observation_correlation_mutation_and_wrong_direction_fail_closed() {
    for mode in [
        "observe_job",
        "observe_scope",
        "observe_load",
        "observe_begin",
        "observe_profile",
        "observe_challenge",
        "observe_payload",
        "observe_direction",
        "observe_ordinary",
    ] {
        let plan = plan();
        let fixture = Fixture::new(mode, &plan);
        let (_owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        let observer = loaded.cleanup_observer();
        loaded
            .begin_observation(binding(&loaded), &cancellation)
            .unwrap();
        assert!(
            await_response(&mut loaded, &cancellation).is_err(),
            "{mode}"
        );
        assert_ne!(observer.status(), CleanupStatus::Pending);
        assert!(loaded.observed_instance().is_none());
    }
}
#[test]
fn correlated_rejections_never_return_observation_or_approval() {
    for (name, code) in [
        ("unsupported", ObservationFailure::Unsupported),
        ("invalid_state", ObservationFailure::InvalidState),
        ("limit", ObservationFailure::Limit),
        ("validation", ObservationFailure::Validation),
        ("cancelled", ObservationFailure::Cancelled),
    ] {
        let plan = plan();
        let fixture = Fixture::new(&format!("observe_reject_{name}"), &plan);
        let (_owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        loaded
            .begin_observation(binding(&loaded), &cancellation)
            .unwrap();
        let error = match await_response(&mut loaded, &cancellation) {
            Err(error) => error,
            Ok(_) => panic!("rejected"),
        };
        assert!(matches!(error.cause, LoadingCause::ObservationRejected(actual) if actual == code));
        assert!(loaded.observed_instance().is_none());
    }
}
#[test]
fn capability_and_expected_loading_binding_checked_before_enqueue() {
    for mode in ["observe_missing", "observe_ok"] {
        let plan = plan();
        let fixture = Fixture::new(mode, &plan);
        let (_owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        let mut expected = binding(&loaded);
        if mode == "observe_ok" {
            expected.begin_binding = ContextDigest::of(b"wrong");
        }
        assert!(loaded.begin_observation(expected, &cancellation).is_err());
        assert_eq!(fixture.records().len(), 2);
    }
}
#[test]
fn observation_keeps_retained_deadline_and_cancellation_interruptible() {
    for cancel in [false, true] {
        let mut plan = plan();
        plan.limits.retained_timeout = Duration::from_millis(120);
        let fixture = Fixture::new("observe_stall", &plan);
        let (owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        let deadline = loaded.deadline;
        thread::sleep(Duration::from_millis(60));
        loaded
            .begin_observation(binding(&loaded), &cancellation)
            .unwrap();
        assert_eq!(loaded.deadline, deadline);
        if cancel {
            owner.send(CancelMode::Hard).unwrap();
        }
        let error = match await_response(&mut loaded, &cancellation) {
            Err(error) => error,
            Ok(_) => panic!("must stop"),
        };
        assert!(matches!(
            (cancel, error.cause),
            (false, LoadingCause::RetainedTimeout) | (true, LoadingCause::Cancelled)
        ));
        assert!(Instant::now() < deadline + Duration::from_secs(1));
    }
}
#[test]
fn duplicate_observation_and_second_job_cannot_be_silently_accepted() {
    for mode in ["observe_duplicate", "observe_ok"] {
        let plan = plan();
        let fixture = Fixture::new(mode, &plan);
        let (_owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        loaded
            .begin_observation(binding(&loaded), &cancellation)
            .unwrap();
        let _frame = await_response(&mut loaded, &cancellation).unwrap();
        if mode == "observe_ok" {
            assert!(
                loaded
                    .begin_observation(binding(&loaded), &cancellation)
                    .is_err()
            );
        } else {
            loop {
                if loaded.poll(&cancellation).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
        assert_eq!(fixture.records().len(), 3);
    }
}

#[test]
fn inspect_only_bridge_can_observe_without_advertising_continuation() {
    let mut plan = plan();
    plan.purpose = LoadingPurpose::Inspect;
    let fixture = Fixture::new("observe_inspect", &plan);
    let (_owner, cancellation) = mpsc::channel();
    let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
    assert_eq!(loaded.purpose(), LoadingPurpose::Inspect);
    loaded
        .begin_observation(binding(&loaded), &cancellation)
        .unwrap();
    let frame = await_response(&mut loaded, &cancellation).unwrap();
    assert!(frame.payload().unwrap().is_some());
    assert_eq!(loaded.purpose(), LoadingPurpose::Inspect);
    assert_eq!(fixture.records().len(), 3);
    loaded.close().unwrap();
}
