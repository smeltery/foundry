mod support_restore;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_store::{BeginRestoreResult, ContinueDecision, RestoreClose, RestorePhase, Store};
use std::sync::{Arc, Barrier};
use support_restore::{begin, capture, continuation, request};
use tempfile::tempdir;

#[test]
fn ordinary_admission_and_restore_loading_cannot_both_acquire_ownership() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let version = store.get(&id).unwrap().version;
    let barrier = Arc::new(Barrier::new(2));
    let restore_barrier = barrier.clone();
    let restore_id = id.clone();
    let value = request(&store, &id, &checkpoint, "load");
    let mut loader = Store::open(&path).unwrap();
    let loading = std::thread::spawn(move || {
        restore_barrier.wait();
        loader.begin_restore(&restore_id, value)
    });
    barrier.wait();
    let ordinary = store.begin_attempt(&id, version);
    let restore = loading.join().unwrap();
    assert_ne!(ordinary.is_ok(), restore.is_ok());
    if let Ok(BeginRestoreResult::Started { fence, .. }) = restore {
        assert_eq!(
            store.work_revision(&id).unwrap(),
            checkpoint.cut.work_revision
        );
        store.close_restore(&fence, RestoreClose::Closed).unwrap();
    } else {
        assert_eq!(
            store.work_revision(&id).unwrap(),
            checkpoint.cut.work_revision + 1
        );
    }
}

#[test]
fn same_and_different_continue_races_issue_exactly_one_permit() {
    for same_request in [true, false] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (id, checkpoint) = capture(&mut store);
        let fence = Arc::new(begin(&mut store, &id, &checkpoint, "load"));
        store.confirm_restored(&fence, [7; 32]).unwrap();
        let before = store.get(&id).unwrap();
        let revision = store.work_revision(&id).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|n| {
                let mut worker = Store::open(&path).unwrap();
                let fence = fence.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let value = continuation(if same_request || n == 0 {
                        "go"
                    } else {
                        "other"
                    });
                    barrier.wait();
                    worker.authorize_continue(&fence, value)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Ok(ContinueDecision::Granted { .. })))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Ok(ContinueDecision::AlreadyAuthorized(_))))
                .count(),
            usize::from(same_request)
        );
        assert_eq!(
            results.iter().filter(|r| r.is_err()).count(),
            usize::from(!same_request)
        );
        assert_eq!(store.get(&id).unwrap().version, before.version + 1);
        assert_eq!(store.work_revision(&id).unwrap(), revision + 1);
        assert_eq!(
            store.restore_request(&id, "load").unwrap().unwrap().phase,
            RestorePhase::Continuing
        );
    }
}

#[test]
fn cancellation_racing_transfer_is_owned_by_loading_or_execution_never_lost() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (id, checkpoint) = capture(&mut store);
    let fence = begin(&mut store, &id, &checkpoint, "load");
    store.confirm_restored(&fence, [7; 32]).unwrap();
    let before = store.get(&id).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_id = id.clone();
    let mut canceller = Store::open(&path).unwrap();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        canceller
            .request_cancel(&worker_id, CancelMode::Hard)
            .unwrap();
    });
    barrier.wait();
    let result = store.authorize_continue(&fence, continuation("go"));
    worker.join().unwrap();
    match result {
        Ok(ContinueDecision::Granted { permit, .. }) => {
            let attempt = permit.into_attempt_fence();
            assert_eq!(
                store.cancellation(&attempt).unwrap(),
                Some(CancelMode::Hard)
            );
            store
                .finish_attempt(&attempt, Lifecycle::Suspended, None, None)
                .unwrap();
        }
        Err(_) => {
            assert_eq!(
                store.restore_cancellation(&fence).unwrap(),
                Some(CancelMode::Hard)
            );
            assert_eq!(store.get(&id).unwrap(), before);
            assert!(store.begin_attempt(&id, before.version).is_err());
            store.close_restore(&fence, RestoreClose::Closed).unwrap();
        }
        _ => panic!("first authorization cannot be a retry"),
    }
}
