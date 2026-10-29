use super::*;
use crate::{control::RunControl, permission::*};
use foundry_contracts::{CancelMode, CreateSession, context::ContextFormat, permission::*};
use foundry_runtime::{AdapterSpec, coordinated::*};
use foundry_tools::host;
use std::{
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    thread,
    time::Duration,
};

struct NoPublication;
impl NativePublicationComparator for NoPublication {
    fn compare(
        &self,
        _: &NativePublicationJob,
    ) -> std::result::Result<
        NativePublicationComparison,
        crate::native_validation::NativeValidationFailure,
    > {
        panic!("permission stop must not request native publication")
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    store: Store,
    fence: AttemptFence,
    publication: Publication,
    stop: CaptureActionRequired,
    requested: Arc<AtomicU8>,
    revocation: ToolPermissionRevocation,
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("store.db")).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "inspect".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let (fence, _) = store
        .begin_fresh_attempt(&session.id, session.version)
        .unwrap()
        .into_parts();
    let policy = TrustedToolPolicy::new(
        "/usr/bin/true".into(),
        cap_std::fs::Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority()).unwrap(),
        vec![host::Grant::Read],
        host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 4096,
            write_bytes: 4096,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 10,
        },
        Duration::from_secs(1),
    )
    .unwrap()
    .require_authorization();
    let permissions = ToolPermissions::new(
        policy.identity(),
        ToolPermissionSettings {
            profile: PermissionProfile::Current,
            configuration_digest: [7; 32],
            generation: 1,
            grants: vec![],
            missing_grant: PermissionDecision::Pending,
            pending_mode: PendingMode::ActionRequired,
            request_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    let revocation = permissions.revocation();
    let requested = Arc::new(AtomicU8::new(0));
    let signals = RunControl::new(Arc::clone(&requested));
    signals
        .permissions
        .initialize(Instant::now() + Duration::from_secs(10));
    let (cancel, cancellation) = mpsc::channel();
    let id = |value| ContextId::new(value).unwrap();
    let (mut publication, ports) = Publication::new(
        &store,
        &fence,
        PublicationConfiguration {
            profile: ProfileBinding {
                stored: StoredFormat {
                    format_id: "fixture".into(),
                    format_version: 1,
                    profile_id: "fixture".into(),
                    binding_id: "fixture".into(),
                    binding_version: "1".into(),
                },
                wire: ContextFormat {
                    profile: id("fixture"),
                    format_version: id("1"),
                    producer_build: id("fixture"),
                },
                configuration_digest: [7; 32],
            },
            policy: b"fixture".to_vec(),
            associations: b"fixture".to_vec(),
            validator: NativePublicationRegistration {
                identity: ContextDigest::of(b"fixture"),
                timeout: Duration::from_secs(1),
                comparator: Arc::new(NoPublication),
            },
        },
        WorkerControl {
            signals,
            permissions: Some(Arc::new(permissions)),
            activate_on_ready: false,
            expired: Arc::new(AtomicBool::new(false)),
            cancellation: cancel,
        },
        None,
        None,
    )
    .unwrap();
    let (sent, received) = mpsc::channel();
    let runtime = thread::spawn(move || {
        let mut spec = AdapterSpec::new("/usr/bin/python3");
        spec.args = vec![
            "-u".into(),
            "-c".into(),
            include_str!("../../../tests/publication_owner/permission_adapter.py").into(),
        ];
        spec.run_timeout = Duration::from_secs(3);
        let (_sender, messages) = mpsc::channel();
        sent.send(run_fresh_capture(
            &spec,
            &ports.session,
            &ports.attempt_id,
            &cancellation,
            &messages,
            ports.persistence,
            FreshCaptureOptions {
                profile: ports.profile,
                owner: ports.owner,
                policy: Some(policy),
                phase_timeout: Duration::from_secs(1),
            },
        ))
        .ok()
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(4);
    let stop = loop {
        publication.poll(&mut store, &fence).unwrap();
        if let Ok(result) = received.try_recv() {
            let CaptureExit::ActionRequired(stop) = result.unwrap() else {
                panic!()
            };
            break *stop;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    };
    runtime.join().unwrap();
    Fixture {
        directory,
        store,
        fence,
        publication,
        stop,
        requested,
        revocation,
    }
}

#[test]
fn terminal_admission_rejects_prior_local_cancel_revoke_and_gate_expiry() {
    for change in ["cancel", "revoke", "expired"] {
        let mut h = fixture();
        match change {
            "cancel" => h.requested.store(2, Ordering::Release),
            "revoke" => h.revocation.invalidate(),
            _ => {
                h.publication.control.signals.permissions =
                    crate::permission_gate::PermissionGate::new(Arc::clone(&h.requested));
                h.publication
                    .control
                    .signals
                    .permissions
                    .initialize(Instant::now());
            }
        }
        assert!(
            h.publication
                .admit_action_required(&h.store, &h.fence, h.stop)
                .is_err()
        );
        assert!(h.store.get(&h.fence.session_id).unwrap().outcome.is_none());
    }
}

#[test]
fn held_sqlite_writer_distinguishes_late_local_control_from_durable_cancel() {
    for change in [
        "local_cancel",
        "revoke",
        "conservative_expiry",
        "durable_cancel",
        "store_expiry",
    ] {
        let mut h = fixture();
        let admitted = h
            .publication
            .admit_action_required(&h.store, &h.fence, h.stop)
            .unwrap();
        if change == "durable_cancel" {
            h.store
                .request_cancel(&h.fence.session_id, CancelMode::Hard)
                .unwrap();
        }
        let writer = rusqlite::Connection::open(h.directory.path().join("store.db")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (entered, entering) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let session_id = h.fence.session_id.clone();
        let worker = thread::spawn(move || {
            entered.send(()).unwrap();
            let outcome = admitted.finish(&mut h.store, &h.fence);
            finished.send((outcome, h.store)).ok().unwrap();
        });
        entering.recv_timeout(Duration::from_secs(1)).unwrap();
        // This barrier is AFTER successful production admission. The real Store
        // finish cannot complete while the independent Immediate writer is held.
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(80)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        match change {
            "local_cancel" => {
                h.requested.store(2, Ordering::Release);
                h.publication
                    .control
                    .signals
                    .relay(&h.publication.control.cancellation);
            }
            "revoke" => h.revocation.invalidate(),
            "conservative_expiry" => {
                h.publication.control.signals.stop_admission();
                h.publication.control.expired.store(true, Ordering::Release);
            }
            "store_expiry" => {
                writer
                    .execute(
                        "UPDATE attempts SET expires_ms=0 WHERE session_id=?1",
                        [&session_id],
                    )
                    .unwrap();
            }
            _ => {}
        }
        // Revocation and local control above finish while SQL remains blocked:
        // no permission/configuration mutex is retained through the transaction.
        writer.execute_batch("COMMIT").unwrap();
        let (outcome, store) = result.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        let session = store.get(&session_id).unwrap();
        if matches!(change, "durable_cancel" | "store_expiry") {
            assert!(outcome.is_err(), "{change}");
            assert!(session.active_attempt_id.is_some());
            assert!(session.outcome.is_none());
        } else {
            assert_eq!(outcome.unwrap().outcome, Some(Outcome::ActionRequired));
            assert_eq!(session.lifecycle, Lifecycle::Exit);
            assert!(session.active_attempt_id.is_none());
        }
        assert!(store.checkpoint_head(&session_id).unwrap().is_none());
    }
}
