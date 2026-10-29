use super::*;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_service::{cancellation_snapshot::SnapshotSettings, restoration_owner::*};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

fn helper() -> PathBuf {
    std::env::var_os("FOUNDRY_SNAPSHOT_TEST_HELPER")
        .expect("rebuilt fixed toolhost required")
        .into()
}
fn workspace(root: &std::path::Path) -> PathBuf {
    let path = root.join("workspace");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("untracked"), b"uncommitted contents").unwrap();
    path.canonicalize().unwrap()
}
fn snapshot_event(store: &Store, session: &str) -> serde_json::Value {
    let events = store.events(session, 0, 100).unwrap();
    let events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "workspace_snapshot")
        .collect();
    assert_eq!(events.len(), 1);
    assert!(!events[0].data.to_string().contains("/"));
    events[0].data.clone()
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn fresh_cancel_records_actual_snapshot_or_limit_and_hard_skips() {
    for mode in ["capture", "limit", "hard", "upgrade"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let path = root.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "fixture".into(),
                directory: workspace.display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let mut settings = SnapshotSettings::for_database(helper(), &path).unwrap();
        settings.destination = settings.destination.canonicalize().unwrap();
        settings.limits.timeout_ms = 5000;
        if mode == "limit" {
            settings.limits.bytes = 1;
        }
        let marker = root.path().join("snapshot-started");
        if mode == "upgrade" {
            let script = root.path().join("slow-helper");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\n/usr/bin/touch '{}'\n/bin/sleep 3\nexec '{}' \"$@\"\n",
                    marker.display(),
                    helper().display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            settings.helper = script;
        }
        let mut owner = FreshCaptureOwner::start_with_settings(
            path.clone(),
            session.id.clone(),
            configuration(),
            RestoreOwnerOptions {
                lease: ServiceOptions {
                    lease_duration: Duration::from_secs(1),
                    poll_interval: Duration::from_millis(20),
                },
                cleanup_timeout: Duration::from_millis(200),
            },
            None,
            Some(settings.clone()),
        )
        .unwrap();
        let FreshCaptureReply::Ready(ports) = receive(&mut owner) else {
            panic!()
        };
        let cancellation = owner.take_cancellation().unwrap();
        owner.cancel(if mode == "hard" {
            CancelMode::Hard
        } else {
            CancelMode::Graceful
        });
        let (_sender, messages) = mpsc::channel();
        // Actual runtime no-spawn branch, not a fabricated cleanup receipt.
        let result = run_fresh_capture(
            &AdapterSpec::new("/not-started"),
            &ports.session,
            &ports.attempt_id,
            &cancellation,
            &messages,
            ports.persistence,
            FreshCaptureOptions {
                profile: ports.profile,
                owner: ports.owner,
                policy: None,
                phase_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let CaptureExit::Cancelled(stop) = result else {
            panic!()
        };
        owner.finish_cancelled(*stop).unwrap();
        if mode == "upgrade" {
            let end = Instant::now() + Duration::from_secs(3);
            while !marker.exists() {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(2));
            }
            // This exceeds both original cleanup allowance and initial durable lease.
            thread::sleep(Duration::from_millis(1100));
            assert!(store.get(&session.id).unwrap().active_attempt_id.is_some());
            owner.cancel(CancelMode::Hard);
        }
        let FreshCaptureReply::Cancelled(done) = receive(&mut owner) else {
            panic!()
        };
        assert_eq!(done.lifecycle, Lifecycle::Suspended);
        assert!(store.checkpoint_head(&session.id).unwrap().is_none());
        let report = owner.shutdown();
        report.operation.unwrap();
        report.release.unwrap();
        assert!(!report.expired);
        if mode == "hard" {
            assert!(
                store
                    .events(&session.id, 0, 100)
                    .unwrap()
                    .iter()
                    .all(|e| e.kind != "workspace_snapshot")
            );
        } else {
            let event = snapshot_event(&store, &session.id);
            if mode == "capture" {
                assert_eq!(event["status"], "captured");
                assert_eq!(
                    fs::read(
                        settings
                            .destination
                            .join(event["artifact"].as_str().unwrap())
                            .join("tree/untracked")
                    )
                    .unwrap(),
                    b"uncommitted contents"
                );
            } else {
                assert_eq!(event["status"], "failed");
                assert_eq!(
                    event["code"],
                    if mode == "limit" {
                        "limit"
                    } else {
                        "aborted_hard"
                    }
                );
            }
        }
    }
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn restored_cancel_after_s2_snapshots_without_replacing_native_head() {
    for upgrade in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let fixture =
            restoration_support::Fixture::with_workspace(Duration::from_secs(3), Some(&workspace));
        let mut settings = SnapshotSettings::for_database(helper(), &fixture.path).unwrap();
        settings.destination = settings.destination.canonicalize().unwrap();
        let marker = root.path().join("snapshot-started");
        if upgrade {
            let script = root.path().join("slow-restored-helper");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\n/usr/bin/touch '{}'\n/bin/sleep 3\nexec '{}' \"$@\"\n",
                    marker.display(),
                    helper().display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            settings.helper = script;
        }
        let mut store = Store::open(&fixture.path).unwrap();
        let before = fixture.head.clone();
        let witness = store
            .read_checkpoint_witness(&fixture.session, &before.id)
            .unwrap()
            .digest();
        let owner = RestorationOwner::start_with_snapshot(
            fixture.path.clone(),
            fixture.session.clone(),
            foundry_store::BeginRestoreInput {
                request_id: "load".into(),
                checkpoint_id: before.id.clone(),
                expected_session_version: store.get(&fixture.session).unwrap().version,
                configuration_digest: before.configuration_digest,
            },
            fixture.options,
            settings.clone(),
        )
        .unwrap();
        let mut h = restoration_support::Harness::with_owner(fixture, owner, 0, "continue_quiet");
        h.validated();
        let message = store
            .enqueue_message(&h.fixture.session, "next", "continue")
            .unwrap();
        h.owner
            .open_continuation(ContinueSelection {
                request_id: "next".into(),
                message_id: message.id,
                sequence: message.sequence,
                text_digest: ContextDigest::of(message.text.as_bytes()),
            })
            .unwrap();
        let RestoreReply::Continuation(endpoint) =
            restoration_support::receive(&mut h.owner).result.unwrap()
        else {
            panic!()
        };
        let mut pending = h
            .loaded
            .continue_with(h.binding, endpoint, &h.cancelled)
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while !pending.poll(&h.cancelled).unwrap() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            store
                .events(&h.fixture.session, 0, 100)
                .unwrap()
                .iter()
                .any(|e| e.kind == "continue_accepted")
        );
        h.owner.cancel(CancelMode::Graceful);
        pending.close().unwrap();
        let retained = h.owner.cancellation_handle();
        let shutdown = thread::spawn(move || h.owner.shutdown());
        if upgrade {
            let end = Instant::now() + Duration::from_secs(3);
            while !marker.exists() {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(2));
            }
            retained.cancel(CancelMode::Hard);
        }
        let report = shutdown.join().unwrap();
        report.release.expect(if upgrade {
            "hard-upgrade snapshot release failed"
        } else {
            "graceful snapshot release failed"
        });
        let done = store.get(&h.fixture.session).unwrap();
        assert_eq!(done.lifecycle, Lifecycle::Suspended);
        assert!(done.active_attempt_id.is_none());
        assert_eq!(
            store.checkpoint_head(&h.fixture.session).unwrap().unwrap(),
            before
        );
        assert_eq!(
            store
                .read_checkpoint_witness(&h.fixture.session, &before.id)
                .unwrap()
                .digest(),
            witness
        );
        let event = snapshot_event(&store, &h.fixture.session);
        if upgrade {
            assert_eq!(event["status"], "failed");
            assert_eq!(event["code"], "aborted_hard");
        } else {
            assert_eq!(event["status"], "captured");
            assert_eq!(
                fs::read(
                    settings
                        .destination
                        .join(event["artifact"].as_str().unwrap())
                        .join("tree/untracked")
                )
                .unwrap(),
                b"uncommitted contents"
            );
        }
    }
}
