use super::*;
use foundry_contracts::Outcome;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Authorization {
    identity: ToolPolicyIdentity,
    count: Arc<AtomicUsize>,
    revoked: Option<ToolAdmissionFailure>,
    grants: Vec<host::Grant>,
}
impl ToolDispatchAuthorization for Authorization {
    fn consume(
        self: Box<Self>,
        current: &ToolPolicyIdentity,
        request: &host::Request,
    ) -> Result<ToolDispatchGrant, ToolAdmissionFailure> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if !current.same_policy(&self.identity)
            || request.attempt_id != "attempt"
            || request.call_id != "call"
            || request.tool != "read"
            || !matches!(&request.input, host::Input::Path(args) if args.path == "item")
        {
            return Err(ToolAdmissionFailure::Invalid);
        }
        if let Some(failure) = self.revoked {
            return Err(failure);
        }
        ToolDispatchGrant::new(self.grants)
    }
}
fn authorized(
    run: &Harness,
    checkpoint: CheckpointId,
    failure: Option<ToolAdmissionFailure>,
    grants: Vec<host::Grant>,
) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    run.replies
        .send(PersistenceReply::ToolAuthorized {
            checkpoint,
            authorization: Box::new(Authorization {
                identity: run.identity.as_ref().unwrap().clone(),
                count: count.clone(),
                revoked: failure,
                grants,
            }),
        })
        .unwrap();
    count
}
#[test]
fn consuming_claim_starts_once_and_waits_for_receipt() {
    let fixture = Fixture::new("ok");
    let run = fixture.start_permissions(vec![host::Grant::Read]);
    let checkpoint = run.claim();
    let count = authorized(&run, checkpoint, None, vec![host::Grant::Read]);
    assert!(matches!(
        run.next(),
        PersistenceRequest::FinishTool {
            evidence: ToolEvidence::Replied(_),
            ..
        }
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(fixture.root.join("effects")).unwrap(), b"x");
    assert!(!fixture.root.join("adapter-result").exists());
    run.replies
        .send(PersistenceReply::ToolFinished {
            checkpoint,
            disposition: ToolDisposition::Success,
        })
        .unwrap();
    run.event();
    assert!(run.finish().is_ok());
}
#[test]
fn required_mode_rejects_bare_claim() {
    let fixture = Fixture::new("ok");
    let run = fixture.start_permissions(vec![host::Grant::Read]);
    run.claimed(run.claim());
    assert!(matches!(
        run.finish(),
        Err(RuntimeError::Protocol("tool authorization required"))
    ));
    assert!(!fixture.root.join("helper-started").exists());
}
#[test]
fn revoked_and_invalid_grants_record_no_spawn_then_stop() {
    for failure in [
        Some(ToolAdmissionFailure::Cancelled),
        Some(ToolAdmissionFailure::Expired),
        Some(ToolAdmissionFailure::Stale),
        Some(ToolAdmissionFailure::Invalid),
        None,
    ] {
        let fixture = Fixture::new("ok");
        let run = fixture.start_permissions(vec![host::Grant::Read]);
        let checkpoint = run.claim();
        let grants = if failure.is_none() {
            vec![host::Grant::List]
        } else {
            vec![host::Grant::Read]
        };
        let count = authorized(&run, checkpoint, failure, grants);
        assert!(matches!(
            run.next(),
            PersistenceRequest::FinishTool {
                evidence: ToolEvidence::FailedBeforeSpawn,
                ..
            }
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(!fixture.root.join("helper-started").exists());
        assert!(run.results.try_recv().is_err());
        run.replies
            .send(PersistenceReply::ToolFinished {
                checkpoint,
                disposition: ToolDisposition::NoEffectFailure,
            })
            .unwrap();
        assert!(matches!(
            run.finish(),
            Err(RuntimeError::Protocol("tool permission admission failed"))
        ));
        assert!(!fixture.root.join("adapter-result").exists());
    }
}
#[test]
fn missing_grant_reaches_owner_and_pending_stops_without_receipt() {
    for disposition in [
        PermissionDisposition::ActionRequired,
        PermissionDisposition::Unsupported,
    ] {
        let fixture = Fixture::new("ok");
        let run = fixture.start_permissions(vec![]);
        let checkpoint = run.claim();
        run.replies
            .send(PersistenceReply::ToolBlocked {
                checkpoint,
                disposition,
            })
            .unwrap();
        let result = run.results.recv_timeout(Duration::from_secs(3)).unwrap();
        match disposition {
            PermissionDisposition::ActionRequired => assert_eq!(
                result.unwrap(),
                RunResult::Completed {
                    outcome: Outcome::ActionRequired,
                    result: "permission_action_required".into(),
                }
            ),
            _ => assert!(matches!(
                result,
                Err(RuntimeError::Protocol("permission_unsupported"))
            )),
        }
        assert!(run.receive.try_recv().is_err());
        assert!(!fixture.root.join("helper-started").exists());
        assert!(!fixture.root.join("adapter-result").exists());
    }
}
#[test]
fn explicit_denial_continues_without_helper_or_finish_receipt() {
    let fixture = Fixture::new("ok");
    let run = fixture.start_permissions(vec![]);
    let checkpoint = run.claim();
    run.replies
        .send(PersistenceReply::ToolBlocked {
            checkpoint,
            disposition: PermissionDisposition::Denied,
        })
        .unwrap();
    assert!(matches!(
        run.event(),
        AdapterEvent::Settled {
            outcome: Outcome::Success,
            ..
        }
    ));
    assert!(run.finish().is_ok());
    assert!(!fixture.root.join("helper-started").exists());
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.root.join("adapter-result")).unwrap())
            .unwrap();
    assert_eq!(
        result["result"],
        serde_json::json!({"status":"denied","code":"denied"})
    );
}
#[test]
fn authorization_debug_is_value_free_and_cancellation_drops_queued_authority() {
    let fixture = Fixture::new("ok");
    let run = fixture.start_permissions(vec![host::Grant::Read]);
    let checkpoint = run.claim();
    let count = Arc::new(AtomicUsize::new(0));
    let reply = PersistenceReply::ToolAuthorized {
        checkpoint,
        authorization: Box::new(Authorization {
            identity: run.identity.as_ref().unwrap().clone(),
            count: count.clone(),
            revoked: None,
            grants: vec![host::Grant::Read],
        }),
    };
    assert_eq!(format!("{reply:?}"), "ToolAuthorized");
    run.cancel.send(CancelMode::Hard).unwrap();
    let _ = run.replies.try_send(reply);
    assert!(matches!(run.finish(), Ok(RunResult::Cancelled { .. })));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(!fixture.root.join("helper-started").exists());
}
