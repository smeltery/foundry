use super::*;
#[test]
fn failed_before_lifecycle_start_requests_explicit_evidence_with_no_digest() {
    let policy = TrustedToolPolicy::new(
        std::env::temp_dir().join("unused-helper"),
        cap_std::fs::Dir::open_ambient_dir(std::env::temp_dir(), cap_std::ambient_authority())
            .unwrap(),
        vec![host::Grant::Read],
        host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 262144,
            write_bytes: 262144,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 256,
        },
        Duration::from_secs(1),
    )
    .unwrap();
    let request = policy
        .prepare(
            "attempt",
            "call",
            "read",
            serde_json::json!({"path":"file"}),
        )
        .unwrap();
    let mut job = Job::new_capture(request);
    job.pending.take();
    job.reply(
        PersistenceReply::ToolClaimed {
            checkpoint: job.checkpoint,
        },
        &policy,
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    assert!(matches!(
        job.pending.take(),
        Some(PersistenceRequest::FinishToolWithEvidence {
            evidence: ToolEvidence::FailedBeforeSpawn,
            ..
        })
    ));
    assert!(
        job.accept_finished(
            job.checkpoint,
            ToolDisposition::NoEffectFailure,
            Some([1; 32]),
            true
        )
        .is_err()
    );
    job.accept_finished(job.checkpoint, ToolDisposition::NoEffectFailure, None, true)
        .unwrap();
    assert!(job.phase == Phase::Ready);
}

fn permission_policy(grants: Vec<host::Grant>) -> TrustedToolPolicy {
    TrustedToolPolicy::new(
        std::env::temp_dir().join("unused-helper"),
        cap_std::fs::Dir::open_ambient_dir(std::env::temp_dir(), cap_std::ambient_authority())
            .unwrap(),
        grants,
        host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 262144,
            write_bytes: 262144,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 256,
        },
        Duration::from_secs(1),
    )
    .unwrap()
    .require_authorization()
}

#[test]
fn policy_identity_preserves_descriptor_clones_but_not_replacements() {
    let policy = permission_policy(vec![]);
    let cloned = policy.try_clone().unwrap();
    assert!(policy.identity().same_policy(&cloned.identity()));
    let replacement = permission_policy(vec![]);
    assert!(!policy.identity().same_policy(&replacement.identity()));
}

#[test]
fn dispatch_grants_recheck_ceiling_and_distinct_operations() {
    let policy = permission_policy(vec![host::Grant::Read, host::Grant::Create]);
    for (tool, input, grants) in [
        (
            "list",
            serde_json::json!({"path":"item"}),
            vec![host::Grant::Read],
        ),
        (
            "list",
            serde_json::json!({"path":"item"}),
            vec![host::Grant::List],
        ),
        (
            "write",
            serde_json::json!({"path":"item","content":"data","mode":"overwrite"}),
            vec![host::Grant::Create],
        ),
        (
            "write",
            serde_json::json!({"path":"item","content":"data","mode":"overwrite"}),
            vec![host::Grant::Overwrite],
        ),
        ("read", serde_json::json!({"path":"item"}), vec![]),
    ] {
        let mut request = policy.prepare("attempt", "call", tool, input).unwrap();
        assert!(
            policy
                .apply_grant(&mut request, ToolDispatchGrant::new(grants).unwrap())
                .is_err()
        );
    }
    let mut request = policy
        .prepare(
            "attempt",
            "call",
            "write",
            serde_json::json!({"path":"item","content":"data","mode":"create"}),
        )
        .unwrap();
    policy
        .apply_grant(
            &mut request,
            ToolDispatchGrant::new(vec![host::Grant::Create]).unwrap(),
        )
        .unwrap();
    assert_eq!(request.grants, vec![host::Grant::Create]);
    request.tool = "read".into();
    assert!(
        policy
            .apply_grant(
                &mut request,
                ToolDispatchGrant::new(vec![host::Grant::Read]).unwrap()
            )
            .is_err()
    );
    assert!(ToolDispatchGrant::new(vec![host::Grant::Read; 2]).is_err());
    assert!(ToolDispatchGrant::new(vec![host::Grant::Read; 5]).is_err());
}

struct CountAuthorization(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl ToolDispatchAuthorization for CountAuthorization {
    fn consume(
        self: Box<Self>,
        _: &ToolPolicyIdentity,
        _: &host::Request,
    ) -> Result<ToolDispatchGrant, ToolAdmissionFailure> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(ToolAdmissionFailure::Stale)
    }
}

#[test]
fn foreign_early_and_duplicate_authority_cannot_consume() {
    let policy = permission_policy(vec![host::Grant::Read]);
    let request = policy
        .prepare(
            "attempt",
            "call",
            "read",
            serde_json::json!({"path":"item"}),
        )
        .unwrap();
    let mut job = Job::new(request);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reply = |checkpoint| PersistenceReply::ToolAuthorized {
        checkpoint,
        authorization: Box::new(CountAuthorization(calls.clone())),
    };
    let stop = Arc::new(AtomicBool::new(false));
    assert!(
        job.reply(reply(job.checkpoint), &policy, stop.clone())
            .is_err()
    );
    job.pending.take();
    assert!(
        job.reply(reply(CheckpointId::new()), &policy, stop.clone())
            .is_err()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    job.reply(reply(job.checkpoint), &policy, stop.clone())
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    job.pending.take();
    assert!(job.reply(reply(job.checkpoint), &policy, stop).is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(job.helper.is_none());
}
