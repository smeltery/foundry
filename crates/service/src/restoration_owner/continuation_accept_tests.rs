use super::*;
use std::cell::Cell;

#[path = "../../../store/tests/support_acceptance/mod.rs"]
pub(in crate::restoration_owner) mod support;

#[test]
fn actual_s2_is_retained_when_inner_checks_observe_cancellation() {
    for (stage, local) in [(1, false), (2, false), (1, true)] {
        let mut f = support::Fixture::new();
        let head = f.store.checkpoint_head(&f.fence.session_id).unwrap();
        let session = f.fence.session_id.clone();
        let path = f.dir.path().join("db");
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() == stage {
                let mut other = Store::open(&path)?;
                // Callback is reached only after the real atomic S2 commit.
                assert_eq!(
                    other.messages(&session, 0, 1)?[0].state,
                    MessageState::Delivered
                );
                other.request_cancel(&session, CancelMode::Graceful)?;
                if local {
                    return Err(ServiceError::Coordination("restoration cancelled"));
                }
            }
            Ok(())
        };
        let mut acceptance = None;
        let mut accepted = false;
        let result = commit_acceptance(
            &mut f.store,
            &f.fence,
            f.input.clone(),
            &mut acceptance,
            &mut accepted,
            &check,
        );
        assert!(super::super::settlement::cancelled_operation(&result));
        assert!(
            matches!(result, Err(ServiceError::Coordination(message)) if message == if local { "restoration cancelled" } else { "stored restoration cancellation" })
        );
        assert!(accepted);
        assert_eq!(f.lookup().unwrap(), acceptance);
        assert_eq!(f.store.checkpoint_head(&session).unwrap(), head);
        let done = f
            .store
            .finish_attempt(
                &f.fence,
                foundry_contracts::Lifecycle::Suspended,
                None,
                None,
            )
            .unwrap();
        assert!(done.active_attempt_id.is_none());
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM context_continue_acceptances",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}

#[test]
fn postcommit_expiry_cleanup_and_protocol_failures_never_become_cancel_settlement() {
    for message in [
        "restoration lease deadline elapsed",
        "continuation runtime cleanup started",
        "restoration shutdown requested",
        "continuation acceptance mismatch",
    ] {
        for stage in [1, 2] {
            let mut f = support::Fixture::new();
            let calls = Cell::new(0);
            let path = f.dir.path().join("db");
            let session = f.fence.session_id.clone();
            let mut acceptance = None;
            let mut accepted = false;
            let result = commit_acceptance(
                &mut f.store,
                &f.fence,
                f.input.clone(),
                &mut acceptance,
                &mut accepted,
                &|| {
                    calls.set(calls.get() + 1);
                    if calls.get() == stage {
                        // A simultaneous durable cancel must not erase this independent failure.
                        Store::open(&path)?.request_cancel(&session, CancelMode::Hard)?;
                        return Err(ServiceError::Coordination(message));
                    }
                    Ok(())
                },
            );
            assert!(matches!(result, Err(ServiceError::Coordination(actual)) if actual == message));
            assert!(!super::super::settlement::cancelled_operation(&result));
            assert!(accepted);
            assert_eq!(f.lookup().unwrap(), acceptance);
        }
    }
}

#[test]
fn healthy_acceptance_finishes_checks_and_rejected_input_never_runs_them() {
    for invalid in [false, true] {
        let mut f = support::Fixture::new();
        let mut input = f.input.clone();
        if invalid {
            input.observed_tail_receipt_digest[0] ^= 1;
        }
        let calls = Cell::new(0);
        let mut acceptance = None;
        let mut accepted = false;
        let result = commit_acceptance(
            &mut f.store,
            &f.fence,
            input,
            &mut acceptance,
            &mut accepted,
            &|| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result.is_ok(), !invalid);
        assert_eq!(accepted, !invalid);
        assert_eq!(calls.get(), if invalid { 0 } else { 2 });
        assert_eq!(f.lookup().unwrap(), acceptance);
        if !invalid {
            assert_eq!(Some(f.accept().unwrap()), acceptance);
        }
    }
}
