use super::*;
use foundry_contracts::CreateSession;

fn actor() -> (Actor, Arc<AtomicU8>) {
    let (_, jobs) = mpsc::sync_channel(1);
    let (replies, _) = mpsc::sync_channel(1);
    let (cancel, _) = mpsc::channel();
    let (renew, _) = mpsc::channel();
    let requested = Arc::new(AtomicU8::new(0));
    let actor = Actor::new(
        RestoreOwnerOptions {
            lease: ServiceOptions {
                lease_duration: Duration::from_secs(4),
                poll_interval: Duration::from_millis(25),
            },
            cleanup_timeout: Duration::from_secs(2),
        },
        Duration::from_secs(2),
        Channels {
            jobs,
            replies,
            cancel,
            signals: RunControl::new(requested.clone()),
            stopping: Arc::new(AtomicBool::new(false)),
            expired: Arc::new(AtomicBool::new(false)),
            renew,
        },
    );
    (actor, requested)
}

#[test]
fn shutdown_between_loop_check_and_live_exits_without_continuing_work() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("db")).unwrap();
    let (actor, _) = actor();
    // Deterministically pause after the loop's initial false observation. This
    // is the same interleaving as shutdown while a heartbeat is in progress.
    assert!(!actor.io.stopping.load(Ordering::Acquire));
    actor.io.stopping.store(true, Ordering::Release);
    let interrupted = actor.live(&mut store, "unused");
    assert!(matches!(
        interrupted,
        Err(ServiceError::Coordination("restoration shutdown requested"))
    ));
    actor
        .finish_serving(&mut store, "unused", interrupted)
        .unwrap();
    // Normalization must not make a stopped actor live or admit another job.
    assert!(actor.live(&mut store, "unused").is_err());
}

#[test]
fn shutdown_does_not_hide_expiry_cancellation_or_other_failures() {
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
    let (actor, requested) = actor();
    actor.io.stopping.store(true, Ordering::Release);
    let interrupted = actor.live(&mut store, &session.id);
    actor.io.expired.store(true, Ordering::Release);
    assert!(matches!(
        actor.finish_serving(&mut store, &session.id, interrupted),
        Err(ServiceError::Coordination(
            "restoration lease deadline elapsed"
        ))
    ));
    actor.io.expired.store(false, Ordering::Release);
    let interrupted = actor.live(&mut store, &session.id);
    requested.store(2, Ordering::Release);
    assert!(matches!(
        actor.finish_serving(&mut store, &session.id, interrupted),
        Err(ServiceError::Coordination("restoration cancelled"))
    ));
    assert_eq!(store.cancellation(&fence).unwrap(), Some(CancelMode::Hard));
    let failure = Err(ServiceError::Coordination("native selection changed"));
    assert!(matches!(
        actor.finish_serving(&mut store, &session.id, failure),
        Err(ServiceError::Coordination("native selection changed"))
    ));
    let failure = Err(ServiceError::Invalid("storage unavailable"));
    assert!(matches!(
        actor.finish_serving(&mut store, &session.id, failure),
        Err(ServiceError::Invalid("storage unavailable"))
    ));
}

use super::super::continuation::acceptance_tests::support as acceptance_support;

#[test]
fn inner_accept_checks_actual_cancel_before_shutdown_but_preserves_expiry() {
    for (cancelled, expired) in [(true, false), (false, false), (true, true)] {
        let mut f = acceptance_support::Fixture::new();
        let (actor, requested) = actor();
        let path = f.dir.path().join("db");
        let session = f.fence.session_id.clone();
        let mut acceptance = None;
        let mut accepted = false;
        let result = super::super::continuation::commit_acceptance(
            &mut f.store,
            &f.fence,
            f.input.clone(),
            &mut acceptance,
            &mut accepted,
            &|| {
                let mut observed = Store::open(&path)?;
                assert_eq!(
                    observed.messages(&session, 0, 1)?[0].state,
                    foundry_contracts::MessageState::Delivered
                );
                // The CLI can finish runtime cleanup and request shutdown while
                // S2 SQL is returning, before this production callback runs.
                if cancelled {
                    requested.store(1, Ordering::Release);
                    observed.request_cancel(&session, CancelMode::Graceful)?;
                }
                actor.io.stopping.store(true, Ordering::Release);
                actor.io.expired.store(expired, Ordering::Release);
                actor.continuation_authority(None)
            },
        );
        let expected = if expired {
            "restoration lease deadline elapsed"
        } else if cancelled {
            "restoration cancelled"
        } else {
            "continuation owner stopped"
        };
        assert!(matches!(&result, Err(ServiceError::Coordination(actual)) if *actual == expected));
        assert_eq!(
            super::super::settlement::cancelled_operation(&result),
            cancelled && !expired
        );
        assert!(accepted);
        assert_eq!(f.lookup().unwrap(), acceptance);
        if cancelled {
            assert!(
                f.accept().is_err(),
                "cancellation still blocks acceptance retry"
            );
        } else {
            assert_eq!(Some(f.accept().unwrap()), acceptance);
        }
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM context_continue_acceptances",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
}
