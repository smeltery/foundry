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
