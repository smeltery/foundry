//! Synthetic owner protocol fixture. No native semantic validator is installed.
use crate::context_cut::{CaptureEvidence, prepare_cut};
use foundry_contracts::{
    CreateSession,
    context::{ContextDigest, ContextFormat, ContextId},
};
use foundry_runtime::{
    AdapterSpec,
    coordinated::{
        CaptureExit, FreshCaptureOptions, PersistenceReply, PersistenceRequest, PreparedCapture,
        RuntimePersistence,
        capture::{OwnerValidationAttestation, ProfileBinding, StoredFormat},
        capture_owner::{CaptureOwner, CaptureReply, CaptureRequest},
        run_fresh_capture,
    },
};
use foundry_store::{AttemptFence, Store};
use std::{
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

pub fn profile() -> ProfileBinding {
    ProfileBinding {
        stored: StoredFormat {
            format_id: "stored-format".into(),
            format_version: 5,
            profile_id: "stored-profile".into(),
            binding_id: "binding".into(),
            binding_version: "binding-v7".into(),
        },
        wire: ContextFormat {
            profile: ContextId::new("wire-profile").unwrap(),
            format_version: ContextId::new("wire-v3").unwrap(),
            producer_build: ContextId::new("fixture-build").unwrap(),
        },
        configuration_digest: std::array::from_fn(|i| i as u8),
    }
}

pub struct Fixture {
    pub directory: tempfile::TempDir,
    pub store: Store,
    pub fence: AttemptFence,
    pub evidence: CaptureEvidence,
    pub expected: ContextDigest,
    pub prepared: PreparedCapture,
}

pub fn fresh(result: &str) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("store.db")).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "fixture prompt".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let session = store.get(&session.id).unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("adapter.py").into(),
        directory.path().as_os_str().to_owned(),
        serde_json::to_string(result).unwrap().into(),
    ];
    spec.run_timeout = Duration::from_secs(8);
    spec.cancel_timeout = Duration::from_millis(100);
    let (requests, persistence_rx) = mpsc::sync_channel(2);
    let (replies, reply_rx) = mpsc::sync_channel(2);
    let (owner, endpoint) = CaptureOwner::channel();
    let (_cancel, cancellation) = mpsc::channel();
    let (_message, messages) = mpsc::channel();
    let attempt = fence.attempt_id.clone();
    let worker = std::thread::spawn(move || {
        run_fresh_capture(
            &spec,
            &session,
            &attempt,
            &cancellation,
            &messages,
            RuntimePersistence {
                requests,
                replies: reply_rx,
            },
            FreshCaptureOptions {
                profile: profile(),
                owner,
                policy: None,
                phase_timeout: Duration::from_secs(4),
            },
        )
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut evidence = None;
    let mut expected = None;
    let mut event_number = 0;
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "runtime fixture timed out");
        if let Ok(request) = persistence_rx.try_recv() {
            let PersistenceRequest::Event { checkpoint, event } = request else {
                panic!("unexpected tool request")
            };
            event_number += 1;
            store
                .append_event(
                    &fence,
                    &format!("event-{event_number}"),
                    0,
                    "fixture",
                    serde_json::to_value(event).unwrap(),
                )
                .unwrap();
            replies
                .send(PersistenceReply::EventCommitted { checkpoint })
                .unwrap();
        }
        if let Ok(request) = endpoint.requests.try_recv() {
            let reply = match request {
                CaptureRequest::ActivateAdmission { correlation, .. } => {
                    CaptureReply::AdmissionActivated { correlation }
                }
                CaptureRequest::FreezeAdmission { correlation } => {
                    CaptureReply::AdmissionFrozen { correlation }
                }
                CaptureRequest::PrepareCut { correlation } => {
                    let (cut, retained) =
                        prepare_cut(&store, &fence, "capture-request".into(), profile()).unwrap();
                    evidence = Some(retained);
                    CaptureReply::CutPrepared {
                        correlation,
                        cut: Box::new(cut),
                    }
                }
                CaptureRequest::ValidateCandidate { correlation, job } => {
                    // Independently cache the actual job binding before owner acceptance.
                    // This explicitly synthetic assertion tests protocol handoff, not native semantics.
                    expected = Some(job.binding_digest());
                    let verified = job
                        .accept_from_trusted_owner(OwnerValidationAttestation {
                            validator_id: ContextId::new("synthetic-protocol-fixture").unwrap(),
                            validator_version: ContextId::new("1").unwrap(),
                            completed_turn_id: ContextId::new("turn").unwrap(),
                        })
                        .unwrap();
                    CaptureReply::CandidateVerified {
                        correlation,
                        verified: Box::new(verified),
                    }
                }
            };
            endpoint.replies.send(reply).ok().unwrap();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let CaptureExit::Prepared(prepared) = worker.join().unwrap().unwrap() else {
        panic!("expected prepared cleanup handoff")
    };
    let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
    let status = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap();
    assert!(
        !status.status.success(),
        "adapter must be reaped before prepared"
    );
    Fixture {
        directory,
        store,
        fence,
        evidence: evidence.unwrap(),
        expected: expected.unwrap(),
        prepared: *prepared,
    }
}
