mod support_acceptance;
use foundry_contracts::MessageState;
use support_acceptance::Fixture;

#[test]
fn rollback_at_each_write_ignore_and_final_ownership_check() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON events BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON messages BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON messages BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_continue_acceptances BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_continue_acceptances BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_acceptances BEGIN UPDATE attempts SET expires_ms=1; END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_acceptances BEGIN UPDATE attempts SET cancel='hard'; END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_acceptances BEGIN UPDATE context_restores SET phase='finished'; END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_acceptances BEGIN UPDATE messages SET state='in_flight'; END",
        "CREATE TRIGGER fail AFTER INSERT ON events BEGIN UPDATE session_work SET revision=9223372036854775807; END",
    ] {
        let mut f = Fixture::new();
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        f.db().execute_batch(trigger).unwrap();
        assert!(f.accept().is_err(), "{trigger}");
        assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
        assert_eq!(
            f.store.messages(&f.fence.session_id, 0, 10).unwrap()[0].state,
            MessageState::InFlight
        );
        assert!(
            f.store
                .events(&f.fence.session_id, 0, 10)
                .unwrap()
                .is_empty()
        );
        assert!(f.lookup().unwrap().is_none());
        f.db().execute_batch("DROP TRIGGER fail").unwrap();
        f.accept().unwrap();
    }
}
#[test]
fn revision_exhaustion_never_commits_half_an_acceptance() {
    let mut f = Fixture::new();
    f.db()
        .execute("UPDATE session_work SET revision=?1", [i64::MAX - 1])
        .unwrap();
    assert!(f.accept().is_err());
    assert_eq!(
        f.store.work_revision(&f.fence.session_id).unwrap(),
        (i64::MAX - 1) as u64
    );
    assert!(
        f.store
            .events(&f.fence.session_id, 0, 10)
            .unwrap()
            .is_empty()
    );
    assert!(f.lookup().unwrap().is_none());
}
