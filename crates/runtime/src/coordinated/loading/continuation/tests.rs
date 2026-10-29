use super::*;
use crate::coordinated::{PersistenceReply, PersistenceRequest, RuntimePersistence};
use std::{sync::mpsc, thread};

use super::support::{Harness, loaded};

#[test]
fn immediate_output_waits_for_durable_activation_then_uses_same_coordinator() {
    let mut h = Harness::new("continue_ok");
    let pid = h.pending.process_id();
    let observer = h.pending.cleanup_observer();
    let (_, digest) = h.grant();
    let job = h.accepted(digest);
    for _ in 0..10 {
        assert!(!h.pending.poll(&h.cancellation).unwrap());
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(h.fixture.records().len(), 4); // Tool is buffered: no result or Start.
    h.replies
        .send(ContinuationReply::Activated { job })
        .ok()
        .unwrap();
    assert!(h.pending.poll(&h.cancellation).unwrap());
    let admitted = h.pending.into_admitted().unwrap();
    assert_eq!(admitted.process_id(), pid);
    let session = foundry_contracts::Session {
        id: "session".into(),
        prompt: "historical".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: foundry_contracts::Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("new".into()),
        version: 2,
    };
    let (_tx, messages) = mpsc::channel();
    let (requests, rx) = mpsc::sync_channel(2);
    let (tx, replies) = mpsc::sync_channel(2);
    let worker = thread::spawn(move || {
        let mut kinds = Vec::new();
        while let Ok(PersistenceRequest::Event { checkpoint, event }) = rx.recv() {
            kinds.push(match event {
                foundry_contracts::AdapterEvent::Text { .. } => "text",
                foundry_contracts::AdapterEvent::ToolCall { .. } => "tool",
                foundry_contracts::AdapterEvent::Settled { .. } => "settled",
                _ => panic!(),
            });
            tx.send(PersistenceReply::EventCommitted { checkpoint })
                .unwrap();
        }
        kinds
    });
    let result = admitted
        .run_restored(
            &h.fixture.spec,
            &session,
            &h.cancellation,
            &messages,
            RuntimePersistence { requests, replies },
            None,
        )
        .unwrap();
    assert_eq!(worker.join().unwrap(), ["text", "tool", "settled"]);
    assert!(result.binding.baseline.is_restored());
    assert_eq!(result.delta.entries().len(), 2);
    assert!(matches!(
        result.delta.entries()[0].kind(),
        crate::coordinated::capture::TraceKind::Instruction
    ));
    assert_eq!(result.delta.entries()[0].bytes(), b"new instruction");
    assert!(result.delta.entries()[0].instruction_is_durable());
    assert_eq!(observer.status(), CleanupStatus::Complete);
    assert!(
        h.fixture
            .records()
            .iter()
            .all(|record| record["pid"] == pid)
    );
    assert_eq!(h.fixture.records()[4]["kind"], "tool_result");
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
}
#[test]
fn inspection_or_wrong_observation_never_requests_authorization() {
    for purpose in [LoadingPurpose::Inspect, LoadingPurpose::PrepareContinuation] {
        let (_, loaded, mut binding, _cancel, cancellation) = loaded("continue_quiet", purpose);
        if purpose == LoadingPurpose::PrepareContinuation {
            binding.observation.job_id =
                ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap();
        }
        let (requests, rx) = mpsc::sync_channel(2);
        let (_tx, replies) = mpsc::sync_channel(2);
        assert!(
            loaded
                .continue_with(
                    binding,
                    ContinuationOwner { requests, replies },
                    &cancellation
                )
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }
}
#[test]
fn bad_acceptance_early_output_and_buffer_bounds_fail_closed() {
    for mode in [
        "continue_before",
        "continue_receipt",
        "continue_identity",
        "continue_duplicate",
        "continue_wrong_attempt",
        "continue_overflow",
        "continue_bytes",
    ] {
        let mut h = Harness::new(mode);
        let observer = h.pending.cleanup_observer();
        h.grant();
        loop {
            if h.pending.poll(&h.cancellation).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_ne!(observer.status(), CleanupStatus::Pending, "{mode}");
        assert!(
            !h.fixture
                .records()
                .iter()
                .any(|r| r["kind"] == "tool_result")
        );
        assert!(h.pending.into_admitted().is_err());
    }
}
#[test]
fn cancel_timeout_owner_rejection_and_duplicate_reply_cleanup_without_replay() {
    for mode in ["cancel", "expiry", "reject", "duplicate", "disconnect"] {
        let mut h = Harness::new("continue_quiet");
        let observer = h.pending.cleanup_observer();
        let (job, digest) = h.grant();
        if mode == "duplicate" {
            h.replies
                .send(ContinuationReply::Activated { job })
                .ok()
                .unwrap();
        } else {
            let job = h.accepted(digest);
            match mode {
                "cancel" => h.cancel.send(CancelMode::Hard).unwrap(),
                "expiry" => h.pending.loaded.as_mut().unwrap().deadline = Instant::now(),
                "reject" => h
                    .replies
                    .send(ContinuationReply::Rejected { job })
                    .ok()
                    .unwrap(),
                "disconnect" => drop(h.replies),
                _ => unreachable!(),
            }
        }
        assert!(h.pending.poll(&h.cancellation).is_err(), "{mode}");
        assert_ne!(observer.status(), CleanupStatus::Pending);
        assert!(h.fixture.records().len() <= 4);
    }
}
#[test]
fn stalled_acceptance_remains_interruptible_and_bounded() {
    for cancel in [false, true] {
        let mut h = Harness::new("continue_stall");
        h.grant();
        while h.fixture.records().len() < 4 {
            assert!(!h.pending.poll(&h.cancellation).unwrap());
            thread::sleep(Duration::from_millis(2));
        }
        if cancel {
            h.cancel.send(CancelMode::Graceful).unwrap();
        } else {
            h.pending.loaded.as_mut().unwrap().deadline = Instant::now();
        }
        let observer = h.pending.cleanup_observer();
        assert!(h.pending.poll(&h.cancellation).is_err());
        assert_eq!(observer.status(), CleanupStatus::Complete);
        assert!(h.requests.try_recv().is_err());
        assert_eq!(h.fixture.records().len(), 4);
    }
}
#[test]
fn owner_request_backpressure_retains_job_and_original_receipt_digest() {
    let (_, loaded, binding, _cancel, cancellation) =
        loaded("continue_quiet", LoadingPurpose::PrepareContinuation);
    let (requests, rx) = mpsc::sync_channel(1);
    let (_tx, replies) = mpsc::sync_channel(2);
    let blocker = ContinuationJob(uuid::Uuid::new_v4());
    requests
        .send(ContinuationRequest::Authorize {
            job: blocker.clone(),
            binding: Arc::new(binding),
            tail_receipt_digest: ContextDigest::of(b"occupied"),
        })
        .ok()
        .unwrap();
    let ContinuationRequest::Authorize { binding, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    let binding = Arc::try_unwrap(binding).ok().unwrap();
    // Refill before the pending owner gets any chance to submit.
    requests
        .send(ContinuationRequest::Accept {
            job: blocker,
            scope: loaded.scope.clone(),
            identity: ContinuationIdentity {
                attempt_id: ContextId::new("blocker").unwrap(),
                continuation_id: ContextId::new("blocker").unwrap(),
                input_id: ContextId::new("blocker").unwrap(),
                input_sequence: 1,
                input_digest: ContextDigest::of(b"x"),
                input_bytes: 1,
                authorization_binding: ContextDigest::of(b"x"),
            },
            receipt: ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        })
        .ok()
        .unwrap();
    let mut pending = loaded
        .continue_with(
            binding,
            ContinuationOwner { requests, replies },
            &cancellation,
        )
        .unwrap();
    for _ in 0..5 {
        assert!(!pending.poll(&cancellation).unwrap());
    }
    assert!(matches!(
        rx.recv().unwrap(),
        ContinuationRequest::Accept { .. }
    ));
    assert!(!pending.poll(&cancellation).unwrap());
    let ContinuationRequest::Authorize {
        job,
        tail_receipt_digest,
        ..
    } = rx.recv().unwrap()
    else {
        panic!()
    };
    assert!(job == pending.job);
    assert!(tail_receipt_digest == ContextDigest::of(pending.receipt.as_str().as_bytes()));
    assert!(!pending.poll(&cancellation).unwrap());
    assert!(rx.try_recv().is_err());
    pending.close().unwrap();
}
