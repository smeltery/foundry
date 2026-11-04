mod support_restore;
use foundry_contracts::{CancelMode, CancelReceipt, ContractError, Lifecycle, Outcome};
use foundry_store::{RestoreClose, Store, StoreError, TerminalProposal};
use rusqlite::Connection;
use support_restore::{begin, capture, checkpoint, continuation, request};
use tempfile::tempdir;

fn receipt(requested: bool, mode: CancelMode, status: Lifecycle) -> CancelReceipt {
    CancelReceipt {
        ok: true,
        requested,
        mode,
        status,
    }
}

fn cancel(store: &mut Store, id: &str, mode: CancelMode) -> CancelReceipt {
    store.cancel_session(id, mode).unwrap().receipt
}

fn refused<T>(result: foundry_store::Result<T>) {
    match result {
        Err(StoreError::Contract(ContractError::Cancelled(_))) => {}
        Err(other) => panic!("expected a cancelled refusal, got {other}"),
        Ok(_) => panic!("a cancelled session accepted further execution"),
    }
}

#[test]
fn graceful_then_hard_upgrade_while_running_reports_each_receipt() {
    let mut store = Store::open(":memory:").unwrap();
    let id = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    use CancelMode::{Graceful, Hard};
    use Lifecycle::Running;
    for (mode, expected) in [
        (Graceful, receipt(true, Graceful, Running)),
        (Graceful, receipt(false, Graceful, Running)),
        (Hard, receipt(true, Hard, Running)),
        (Hard, receipt(false, Hard, Running)),
        (Graceful, receipt(false, Hard, Running)),
    ] {
        assert_eq!(cancel(&mut store, &id, mode), expected);
    }
    assert_eq!(store.cancellation(&fence).unwrap(), Some(Hard));
    // A live request never stops the attempt or skips its suspended settlement.
    assert!(
        store
            .finish_attempt(&fence, Lifecycle::Cancelled, None, None)
            .is_err()
    );
    let settled = store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    assert_eq!(settled.lifecycle, Lifecycle::Suspended);
}

#[test]
fn cancelling_a_suspended_session_is_terminal_and_blocks_later_continuation() {
    let mut store = Store::open(":memory:").unwrap();
    let id = checkpoint::session(&mut store);
    store.enqueue_message(&id, "early", "follow up").unwrap();
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Hard),
        receipt(true, CancelMode::Hard, Lifecycle::Suspended)
    );
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Graceful),
        receipt(true, CancelMode::Graceful, Lifecycle::Cancelled)
    );
    let cancelled = store.get(&id).unwrap();
    assert_eq!(cancelled.lifecycle, Lifecycle::Cancelled);
    assert!(cancelled.outcome.is_none() && cancelled.active_attempt_id.is_none());
    for mode in [CancelMode::Graceful, CancelMode::Hard] {
        assert_eq!(
            cancel(&mut store, &id, mode),
            receipt(false, mode, Lifecycle::Cancelled)
        );
    }
    assert_eq!(store.get(&id).unwrap(), cancelled);
    refused(store.begin_attempt(&id, cancelled.version));
    assert!(store.begin_fresh_attempt(&id, cancelled.version).is_err());
    refused(store.enqueue_message(&id, "late", "follow up"));
    // An idempotent retry of an already accepted message still reads it back.
    assert_eq!(
        store.enqueue_message(&id, "early", "follow up").unwrap().id,
        "early"
    );
    assert_eq!(store.get(&id).unwrap(), cancelled);
}

#[test]
fn hard_cancel_keeps_the_last_published_checkpoint_and_publishes_nothing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let published = store
        .commit_opaque_checkpoint(&fence, checkpoint::input(&store, &fence, "published"))
        .unwrap();
    let mut pending = checkpoint::input(&store, &fence, "pending");
    pending.expected_head = Some(published.id.clone());
    let finishing = checkpoint::input(&store, &fence, "finishing");
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Hard),
        receipt(true, CancelMode::Hard, Lifecycle::Running)
    );
    assert!(store.checkpoint_cut(&fence).is_err());
    assert!(store.commit_opaque_checkpoint(&fence, pending).is_err());
    let terminal = TerminalProposal {
        lifecycle: Lifecycle::Suspended,
        outcome: None,
        result: None,
    };
    assert!(
        store
            .finish_with_opaque_checkpoint(&fence, finishing, terminal)
            .is_err()
    );
    store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    assert_eq!(store.checkpoint_head(&id).unwrap().unwrap(), published);
    let rows: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM context_checkpoints", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    // The suspended session can still restore from that head until it is cancelled.
    let owner = begin(&mut store, &id, &published, "restore");
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Graceful),
        receipt(true, CancelMode::Graceful, Lifecycle::Suspended)
    );
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Graceful),
        receipt(false, CancelMode::Graceful, Lifecycle::Suspended)
    );
    store.confirm_restored(&owner, [7; 32]).unwrap_err();
    assert!(
        store
            .authorize_continue(&owner, continuation("continue"))
            .is_err()
    );
    store.close_restore(&owner, RestoreClose::Closed).unwrap();
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Hard).status,
        Lifecycle::Cancelled
    );
    refused(store.begin_restore(&id, request(&store, &id, &published, "later")));
    assert_eq!(store.checkpoint_head(&id).unwrap().unwrap(), published);
}

#[test]
fn cancelling_a_settled_session_is_a_reported_no_op() {
    let mut store = Store::open(":memory:").unwrap();
    let (id, _) = capture(&mut store);
    let before = store.get(&id).unwrap();
    assert_eq!(before.outcome, Some(Outcome::Success));
    assert_eq!(
        cancel(&mut store, &id, CancelMode::Graceful),
        receipt(false, CancelMode::Graceful, Lifecycle::Exit)
    );
    assert_eq!(store.get(&id).unwrap(), before);

    let failed = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&failed, 0).unwrap();
    let before = store
        .finish_attempt(&fence, Lifecycle::Error, None, None)
        .unwrap();
    assert_eq!(
        cancel(&mut store, &failed, CancelMode::Hard),
        receipt(false, CancelMode::Hard, Lifecycle::Error)
    );
    assert_eq!(store.get(&failed).unwrap(), before);
}
