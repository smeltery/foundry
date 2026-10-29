use super::*;
use foundry_contracts::{CreateSession, permission::PermissionProfile};
use foundry_runtime::coordinated::{ToolAdmissionFailure, TrustedToolPolicy};
use foundry_store::Store;
use std::sync::atomic::{AtomicU8, Ordering};

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    fence: AttemptFence,
    policy: TrustedToolPolicy,
    permissions: ToolPermissions,
    signals: RunControl,
    cancel: Arc<AtomicU8>,
    intent: ToolIntent,
    request: host::Request,
}
impl Fixture {
    fn new(timeout: Duration) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("db")).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "inspect".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, session.version).unwrap();
        let limits = host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 262144,
            write_bytes: 262144,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 256,
        };
        let policy = TrustedToolPolicy::new(
            directory.path().join("helper"),
            cap_std::fs::Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority())
                .unwrap(),
            vec![host::Grant::Read],
            limits.clone(),
            Duration::from_secs(1),
        )
        .unwrap()
        .require_authorization();
        let permissions = ToolPermissions::new(
            policy.identity(),
            ToolPermissionSettings {
                profile: PermissionProfile::Current,
                configuration_digest: [9; 32],
                generation: 1,
                grants: vec![host::Grant::Read],
                missing_grant: PermissionDecision::Pending,
                pending_mode: PendingMode::ActionRequired,
                request_timeout: timeout,
            },
        )
        .unwrap();
        let cancel = Arc::new(AtomicU8::new(0));
        let signals = RunControl::new(cancel.clone());
        signals
            .permissions
            .initialize(Instant::now() + Duration::from_secs(10));
        let input = host::Input::Path(host::PathArgs {
            path: "note.txt".into(),
        });
        let intent = ToolIntent {
            call_id: "call".into(),
            tool: "read".into(),
            input: input.clone(),
        };
        let request = host::Request {
            version: host::VERSION,
            attempt_id: fence.attempt_id.clone(),
            call_id: intent.call_id.clone(),
            grants: vec![host::Grant::Read],
            limits,
            tool: intent.tool.clone(),
            input,
        };
        Self {
            _directory: directory,
            store,
            fence,
            policy,
            permissions,
            signals,
            cancel,
            intent,
            request,
        }
    }
    fn claim(&mut self) -> Box<dyn ToolDispatchAuthorization> {
        let PreparedPermission::Authorized(authorization) = self
            .permissions
            .prepare(&self.fence, &self.intent, &self.signals)
            .unwrap()
        else {
            panic!("expected trusted authorization")
        };
        self.store
            .claim_tool(
                &self.fence,
                &self.intent.call_id,
                &self.intent.tool,
                tool_digest::input(&self.intent.tool, &self.intent.input).unwrap(),
            )
            .unwrap();
        authorization
    }
}

#[test]
fn bound_input_attempt_call_and_tool_cannot_be_substituted_after_claim() {
    for field in ["attempt", "call", "tool", "input"] {
        let mut f = Fixture::new(Duration::from_secs(2));
        let authorization = f.claim();
        match field {
            "attempt" => f.request.attempt_id = "other-attempt".into(),
            "call" => f.request.call_id = "other-call".into(),
            "tool" => f.request.tool = "list".into(),
            _ => {
                f.request.input = host::Input::Path(host::PathArgs {
                    path: "other.txt".into(),
                })
            }
        }
        assert!(
            matches!(
                authorization.consume(&f.policy.identity(), &f.request),
                Err(ToolAdmissionFailure::Invalid)
            ),
            "{field}"
        );
        assert_eq!(
            f.store
                .tool_call(&f.fence.session_id, "call")
                .unwrap()
                .unwrap()
                .outcome,
            None
        );
    }
}

#[test]
fn current_policy_and_config_revocation_survive_a_durable_claim() {
    for configuration in [false, true] {
        let mut f = Fixture::new(Duration::from_secs(2));
        let authorization = f.claim();
        let other = Fixture::new(Duration::from_secs(2));
        let identity = if configuration {
            // Same profile/config digest values cannot revive an invalidated selection.
            f.permissions.revocation().invalidate();
            f.policy.identity()
        } else {
            other.policy.identity()
        };
        assert!(matches!(
            authorization.consume(&identity, &f.request),
            Err(ToolAdmissionFailure::Stale)
        ));
    }
}

#[test]
fn cancellation_and_expiry_after_claim_cannot_consume() {
    for cancel in [false, true] {
        let mut f = Fixture::new(Duration::from_secs(2));
        let authorization = f.claim();
        if cancel {
            f.cancel.store(1, Ordering::Release);
        } else {
            // Accepted renewal would never provide an elapsed deadline; exercise the
            // final consumer's own clock check without waiting for watchdog scheduling.
            let previous = Instant::now();
            assert!(!f.signals.permissions.renew(previous, previous));
        }
        assert!(matches!(
            authorization.consume(&f.policy.identity(), &f.request),
            Err(ToolAdmissionFailure::Cancelled)
        ));
    }
    let mut f = Fixture::new(Duration::from_millis(1));
    let authorization = f.claim();
    std::thread::sleep(Duration::from_millis(3));
    assert!(matches!(
        authorization.consume(&f.policy.identity(), &f.request),
        Err(ToolAdmissionFailure::Expired)
    ));
}

#[test]
fn dropped_or_consumed_authority_does_not_make_claim_replayable() {
    for consume in [false, true] {
        let mut f = Fixture::new(Duration::from_secs(2));
        let authorization = f.claim();
        if consume {
            assert!(
                authorization
                    .consume(&f.policy.identity(), &f.request)
                    .is_ok()
            );
        } else {
            drop(authorization);
        }
        assert!(
            f.store
                .claim_tool(
                    &f.fence,
                    "call",
                    "read",
                    tool_digest::input("read", &f.intent.input).unwrap()
                )
                .is_err()
        );
        assert_eq!(
            f.store
                .tool_call(&f.fence.session_id, "call")
                .unwrap()
                .unwrap()
                .outcome,
            None
        );
    }
}
