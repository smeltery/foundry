use super::*;
use foundry_contracts::{Lifecycle, Outcome, permission::*};
use foundry_runtime::coordinated::{TrustedToolPolicy, loading::CleanupStatus};
use foundry_service::{permission::*, restoration_owner::*};
use foundry_tools::host;

#[test]
fn restored_pending_consumes_checked_cleanup_without_replacing_checkpoint() {
    let mut h = restoration_support::Harness::new(0, "continue_capture");
    h.validated();
    let mut store = Store::open(&h.fixture.path).unwrap();
    let head = store.checkpoint_head(&h.fixture.session).unwrap().unwrap();
    let witness = store
        .read_checkpoint_witness(&h.fixture.session, &head.id)
        .unwrap();
    let witness_digest = witness.digest();
    let message = store
        .enqueue_message(&h.fixture.session, "next", "inspect")
        .unwrap();
    h.owner
        .open_continuation(ContinueSelection {
            request_id: "pending-capture".into(),
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
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pending.poll(&h.cancelled).unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let admitted = pending.into_admitted().unwrap();
    let observer = admitted.cleanup_observer();
    let mut publication = configuration();
    publication.profile.configuration_digest = raw(ContextDigest::of(b"effective-fixture-policy"));
    let policy = TrustedToolPolicy::new(
        "/usr/bin/true".into(),
        cap_std::fs::Dir::open_ambient_dir(
            h.fixture.directory.path(),
            cap_std::ambient_authority(),
        )
        .unwrap(),
        vec![host::Grant::Read],
        host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 4096,
            write_bytes: 4096,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 10,
        },
        Duration::from_secs(2),
    )
    .unwrap()
    .require_authorization();
    let permissions = ToolPermissions::new(
        policy.identity(),
        ToolPermissionSettings {
            profile: PermissionProfile::Current,
            configuration_digest: publication.profile.configuration_digest,
            generation: 1,
            grants: vec![],
            missing_grant: PermissionDecision::Pending,
            pending_mode: PendingMode::ActionRequired,
            request_timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    h.owner
        .open_capture_with_permissions(publication, permissions)
        .unwrap();
    let RestoreReply::CaptureReady(ports) =
        restoration_support::receive(&mut h.owner).result.unwrap()
    else {
        panic!()
    };
    let (_sender, messages) = mpsc::channel();
    let mut spec = AdapterSpec::new("/usr/bin/python3");
    spec.run_timeout = Duration::from_secs(3);
    let result = admitted
        .run_restored_capture(
            &spec,
            &ports.session,
            &h.cancelled,
            &messages,
            ports.persistence,
            FreshCaptureOptions {
                profile: ports.profile,
                owner: ports.owner,
                policy: Some(policy),
                phase_timeout: Duration::from_secs(2),
            },
        )
        .unwrap();
    let CaptureExit::ActionRequired(stop) = result else {
        panic!("permission proof required")
    };
    assert_eq!(observer.status(), CleanupStatus::Complete);
    assert_eq!(stop.attempt_id(), ports.attempt_id);
    assert!(
        store
            .tool_call(&h.fixture.session, "call")
            .unwrap()
            .is_none()
    );
    h.owner.finish_action_required(*stop).unwrap();
    let RestoreReply::ActionRequired(session) =
        restoration_support::receive(&mut h.owner).result.unwrap()
    else {
        panic!()
    };
    assert_eq!(session.lifecycle, Lifecycle::Exit);
    assert_eq!(session.outcome, Some(Outcome::ActionRequired));
    assert_eq!(
        session.result.as_deref(),
        Some("permission_action_required")
    );
    assert!(session.active_attempt_id.is_none());
    assert_eq!(*session, store.get(&h.fixture.session).unwrap());
    assert_eq!(
        store.checkpoint_head(&h.fixture.session).unwrap().unwrap(),
        head
    );
    assert_eq!(
        store
            .read_checkpoint_witness(&h.fixture.session, &head.id)
            .unwrap()
            .digest(),
        witness_digest
    );
    assert_eq!(
        store
            .restore_request(&h.fixture.session, "load")
            .unwrap()
            .unwrap()
            .phase,
        foundry_store::RestorePhase::Finished
    );
    let wire = std::fs::read_to_string(h.fixture.directory.path().join("wire.jsonl")).unwrap();
    assert!(wire.contains("continue_context_v2"));
    assert!(!wire.contains("context_export"));
    assert!(!wire.contains("tool_result"));
    let report = h.owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();
}
