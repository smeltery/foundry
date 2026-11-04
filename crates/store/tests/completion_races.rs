//! Races, continuation and pre-policy records under foundry.completion.v1.
#[path = "support_completion/mod.rs"]
mod support;
use support::*;

#[test]
fn earlier_attempt_evidence_does_not_carry_into_a_continuation() {
    let (_dir, mut store, id) = open(vec![check("build")]);
    let (first, admission) = admit(&mut store, &id);
    let passed = evidence(
        &store,
        &id,
        &admission,
        &[("build", CriterionStatus::Passed)],
    );
    store
        .record_completion_evidence(&first, passed.clone())
        .unwrap();
    assert_eq!(success(&mut store, &first).outcome, Some(Outcome::Success));
    let (second, _) = admit(&mut store, &id);
    // Replaying the first attempt's evidence is stale for the second.
    store.record_completion_evidence(&second, passed).unwrap();
    assert_eq!(
        success(&mut store, &second).outcome,
        Some(Outcome::ActionRequired)
    );
    let earlier = store
        .completion_decision(&id, &first.attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(earlier.outcome, Outcome::Success);
    assert_eq!(earlier.cause, "criteria_verified");
}

#[test]
fn pending_instruction_at_settlement_requires_action_and_stays_queued() {
    let (_dir, mut store, id) = open(vec![check("build")]);
    let (fence, admission) = admit(&mut store, &id);
    let passed = evidence(
        &store,
        &id,
        &admission,
        &[("build", CriterionStatus::Passed)],
    );
    store.record_completion_evidence(&fence, passed).unwrap();
    let message = store
        .enqueue_message(&id, "later", "one more thing")
        .unwrap();
    assert_eq!(
        success(&mut store, &fence).outcome,
        Some(Outcome::ActionRequired)
    );
    assert_eq!(
        cause(&store, &fence),
        ("default".into(), "newer_instruction_pending".into())
    );
    assert_eq!(store.messages(&id, 0, 10).unwrap(), vec![message]);
}

#[test]
fn cancellation_wins_and_a_later_request_does_not_rewrite_a_decision() {
    let (dir, mut store, id) = open(vec![check("build")]);
    let (fence, admission) = admit(&mut store, &id);
    let passed = evidence(
        &store,
        &id,
        &admission,
        &[("build", CriterionStatus::Passed)],
    );
    store.record_completion_evidence(&fence, passed).unwrap();
    store.request_cancel(&id, CancelMode::Graceful).unwrap();
    let refused = store.finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None);
    assert!(matches!(
        refused,
        Err(StoreError::Contract(ContractError::Conflict(_)))
    ));
    let suspended = store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    assert!(suspended.outcome.is_none());
    assert!(
        store
            .completion_decision(&id, &fence.attempt_id)
            .unwrap()
            .is_none()
    );
    assert!(store.current_completion_decision(&id).unwrap().is_none());
    // Cancelling the suspended session is terminal and still records no decision.
    let cancelled = store.request_cancel(&id, CancelMode::Graceful).unwrap();
    assert_eq!(cancelled.lifecycle, Lifecycle::Cancelled);
    assert!(cancelled.outcome.is_none());
    assert!(store.current_completion_decision(&id).unwrap().is_none());
    let rows: i64 = Connection::open(dir.path().join("db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM completion_decisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);

    let (_dir, mut store, id) = open(Vec::new());
    let fence = store.begin_attempt(&id, 0).unwrap();
    let finished = store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
    assert_eq!(
        store.request_cancel(&id, CancelMode::Hard).unwrap(),
        finished
    );
    assert_eq!(cause(&store, &fence).1, "adapter_failure");
}

#[test]
fn pre_policy_records_keep_their_outcome_and_have_no_decision() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    Connection::open(&path).unwrap().execute_batch(V11).unwrap();
    let mut store = Store::open(&path).unwrap();
    let before = store.get(PRE_POLICY_EXIT).unwrap();
    assert_eq!(before.outcome, Some(Outcome::Success));
    assert!(
        store
            .current_completion_decision(PRE_POLICY_EXIT)
            .unwrap()
            .is_none()
    );
    // A continuation gets its own decision; the earlier record is not rewritten.
    let fence = store
        .begin_attempt(PRE_POLICY_EXIT, before.version)
        .unwrap();
    assert_eq!(
        success(&mut store, &fence).outcome,
        Some(Outcome::ActionRequired)
    );
    let rows: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM completion_decisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}
