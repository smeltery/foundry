mod support_acceptance;
use foundry_store::Store;
use std::sync::{Arc, Barrier};
use support_acceptance::Fixture;
#[test]
fn concurrent_exact_acceptors_share_one_event_and_two_revisions() {
    let mut f = Fixture::new();
    let before = f.store.work_revision(&f.fence.session_id).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = f.dir.path().join("db");
            let fence = f.fence.clone();
            let input = f.input.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store.accept_restore_continuation(&fence, input).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(f.accept().unwrap(), results[0]);
    assert_eq!(f.lookup().unwrap(), Some(results[0].clone()));
    assert_eq!(
        f.db()
            .query_row(
                "SELECT COUNT(*) FROM context_continue_acceptances",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        f.store.work_revision(&f.fence.session_id).unwrap(),
        before + 2
    );
    assert_eq!(f.store.events(&f.fence.session_id, 0, 10).unwrap().len(), 1);
}

#[test]
fn changed_prebound_request_and_cancellation_serialize_without_partial_delivery() {
    for cancel in [false, true] {
        let mut f = Fixture::new();
        let barrier = Arc::new(Barrier::new(2));
        let path = f.dir.path().join("db");
        let session = f.fence.session_id.clone();
        let fence = f.fence.clone();
        let mut other = f.input.clone();
        other.intent.load_id = uuid::Uuid::new_v4().to_string();
        other.authorization.delivery_digest = other.intent.delivery_digest().unwrap();
        other.authorization_binding = other.computed_authorization_binding().unwrap();
        // Self-consistent candidate still cannot replace immutable stored authorization.
        assert!(other.request_digest().is_ok());
        let gate = barrier.clone();
        let thread = std::thread::spawn(move || {
            let mut store = Store::open(path).unwrap();
            gate.wait();
            if cancel {
                store
                    .request_cancel(&session, foundry_contracts::CancelMode::Hard)
                    .unwrap();
            } else {
                assert!(store.accept_restore_continuation(&fence, other).is_err());
            }
        });
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        barrier.wait();
        let result = f.accept();
        thread.join().unwrap();
        let accepted = f.lookup().unwrap();
        if result.is_ok() {
            assert!(accepted.is_some());
            assert_eq!(
                f.store.work_revision(&f.fence.session_id).unwrap(),
                before + 2
            );
        } else {
            assert!(cancel);
            assert!(accepted.is_none());
            assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
        }
    }
}
