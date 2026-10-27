mod support_acceptance;
use foundry_contracts::{Lifecycle, MessageState};
use foundry_store::*;
use support_acceptance::Fixture;

#[test]
fn atomic_delivery_retry_and_historical_reopen_never_reissue_authority() {
    let mut f = Fixture::new();
    let before = f.store.work_revision(&f.fence.session_id).unwrap();
    let session = f.store.get(&f.fence.session_id).unwrap();
    let accepted = f.accept().unwrap();
    assert_eq!(
        f.store.work_revision(&f.fence.session_id).unwrap(),
        before + 2
    );
    assert_eq!(f.store.get(&f.fence.session_id).unwrap(), session);
    assert_eq!(
        f.store.messages(&f.fence.session_id, 0, 10).unwrap()[0].state,
        MessageState::Delivered
    );
    assert_eq!(f.accept().unwrap(), accepted);
    assert_eq!(
        f.store.work_revision(&f.fence.session_id).unwrap(),
        before + 2
    );
    let events = f.store.events(&f.fence.session_id, 0, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, accepted.event_id);
    assert_eq!(events[0].revision, 0);
    assert!(!events[0].data.to_string().contains("next instruction"));
    f.store
        .finish_attempt(&f.fence, Lifecycle::Suspended, None, None)
        .unwrap();
    f.store
        .begin_attempt(
            &f.fence.session_id,
            f.store.get(&f.fence.session_id).unwrap().version,
        )
        .unwrap();
    f.store = Store::open(f.dir.path().join("db")).unwrap();
    assert!(f.accept().is_err());
    assert_eq!(f.lookup().unwrap(), Some(accepted));
}

#[test]
fn live_same_owner_required_even_for_retries_but_expired_lookup_is_readonly() {
    for change in [
        "UPDATE attempts SET expires_ms=1 WHERE active=1",
        "UPDATE attempts SET cancel='hard' WHERE active=1",
        "UPDATE attempts SET active=0 WHERE active=1",
    ] {
        let mut f = Fixture::new();
        let accepted = f.accept().unwrap();
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        f.db().execute_batch(change).unwrap();
        assert!(f.accept().is_err());
        assert_eq!(f.lookup().unwrap(), Some(accepted));
        assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
    }
    let mut f = Fixture::new();
    f.accept().unwrap();
    f.fence.token = uuid::Uuid::new_v4().to_string();
    assert!(f.accept().is_err());
    assert!(f.lookup().unwrap().is_some());
}

#[test]
fn reserved_orphans_at_every_revision_and_ordinary_ack_are_rejected() {
    for revision in [0, 1, 99] {
        let mut f = Fixture::new();
        // Obtain exact sanitized event bytes, then remove only the acceptance and undo delivery.
        let accepted = f.accept().unwrap();
        f.db()
            .execute_batch(
                "DELETE FROM context_continue_acceptances; UPDATE messages SET state='in_flight'",
            )
            .unwrap();
        f.db()
            .execute("UPDATE events SET revision=?1", [revision])
            .unwrap();
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        assert!(f.accept().is_err());
        assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
        assert_eq!(
            f.store.events(&f.fence.session_id, 0, 10).unwrap()[0].id,
            accepted.event_id
        );
    }
    let mut f = Fixture::new();
    f.store
        .acknowledge_message(&f.fence, &f.input.intent.message_id)
        .unwrap();
    assert!(f.accept().is_err());
    assert!(f.lookup().unwrap().is_none());
}

#[test]
fn every_immutable_input_field_is_bound_and_zero_admission_is_invalid() {
    let mut f = Fixture::new();
    let original = f.input.clone();
    let mutations: Vec<fn(&mut ContinueAcceptanceInput)> = vec![
        |v| v.intent.session_id = uuid::Uuid::new_v4().to_string(),
        |v| v.intent.restore_id = uuid::Uuid::new_v4().to_string(),
        |v| v.intent.load_id = uuid::Uuid::new_v4().to_string(),
        |v| v.intent.continuation_id = "other".into(),
        |v| {
            v.intent.baseline_checkpoint_id =
                ContextCheckpointId::parse(&uuid::Uuid::new_v4().to_string()).unwrap()
        },
        |v| v.intent.baseline_record_digest[0] ^= 1,
        |v| v.intent.baseline_witness_digest[0] ^= 1,
        |v| v.intent.baseline_validation_binding[0] ^= 1,
        |v| v.intent.configuration_digest[0] ^= 1,
        |v| v.intent.message_id = "other".into(),
        |v| v.intent.message_sequence += 1,
        |v| v.intent.message_text_digest[0] ^= 1,
        |v| v.intent.message_text_bytes += 1,
        |v| v.intent.tail_receipt_digest[0] ^= 1,
        |v| v.observed_tail_receipt_digest[0] ^= 1,
        |v| v.authorization_binding[0] ^= 1,
        |v| v.authorization.attempt_id = uuid::Uuid::new_v4().to_string(),
        |v| v.authorization.session_version = 0,
        |v| v.authorization.admitted_work_revision = 0,
        |v| v.authorization.session_version += 1,
        |v| v.authorization.admitted_work_revision += 1,
        |v| v.authorization.message = None,
    ];
    for change in &mutations {
        let mut input = original.clone();
        change(&mut input);
        assert!(
            f.store
                .accept_restore_continuation(&f.fence, input)
                .is_err()
        );
    }
    f.accept().unwrap();
    for change in mutations {
        let mut input = original.clone();
        change(&mut input);
        assert!(
            f.store
                .accept_restore_continuation(&f.fence, input)
                .is_err()
        );
    }
    for zero_version in [true, false] {
        let mut input = original.clone();
        if zero_version {
            input.authorization.session_version = 0;
        } else {
            input.authorization.admitted_work_revision = 0;
        }
        assert!(input.computed_authorization_binding().is_err());
        assert!(input.request_digest().is_err());
    }
}
