mod support_restore;
use foundry_store::{RestoreClose, RestorePhase, Store};
use rusqlite::Connection;
use support_restore::{begin, capture, continuation};
use tempfile::tempdir;

#[test]
fn failed_continue_writes_and_late_lease_loss_roll_back_every_admission_effect() {
    for trigger in [
        "BEFORE UPDATE ON session_work BEGIN SELECT RAISE(ABORT,'revision'); END",
        "BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT,'attempt'); END",
        "BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'body'); END",
        "BEFORE UPDATE OF continue_digest ON context_restores BEGIN SELECT RAISE(ABORT,'receipt'); END",
        "BEFORE UPDATE OF phase ON context_restores BEGIN SELECT RAISE(ABORT,'transfer'); END",
        "AFTER INSERT ON attempts BEGIN UPDATE context_restores SET expires_ms=1; END",
        "AFTER UPDATE ON sessions BEGIN UPDATE context_restores SET cancel='hard'; END",
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (id, checkpoint) = capture(&mut store);
        let fence = begin(&mut store, &id, &checkpoint, "load");
        store.confirm_restored(&fence, [7; 32]).unwrap();
        let before = store.get(&id).unwrap();
        let revision = store.work_revision(&id).unwrap();
        let sql = Connection::open(&path).unwrap();
        sql.execute_batch(&format!("CREATE TRIGGER fail {trigger};"))
            .unwrap();
        assert!(
            store
                .authorize_continue(&fence, continuation("go"))
                .is_err(),
            "{trigger}"
        );
        assert_eq!(store.get(&id).unwrap(), before, "{trigger}");
        assert_eq!(store.work_revision(&id).unwrap(), revision, "{trigger}");
        let reference = store.restore_request(&id, "load").unwrap().unwrap();
        assert_eq!(reference.phase, RestorePhase::Restored);
        assert!(reference.continuation.is_none());
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(store.restore_cancellation(&fence).unwrap(), None);
        sql.execute_batch("DROP TRIGGER fail;").unwrap();
        store.close_restore(&fence, RestoreClose::Failed).unwrap();
        assert_eq!(store.get(&id).unwrap(), before);
    }
}

#[test]
fn cut_configuration_and_head_are_rechecked_at_confirmation_and_continue() {
    for gate in ["confirm", "continue"] {
        for corruption in ["revision", "head", "version", "config"] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("db");
            let mut store = Store::open(&path).unwrap();
            let (id, checkpoint) = capture(&mut store);
            let fence = begin(&mut store, &id, &checkpoint, "load");
            if gate == "continue" {
                store.confirm_restored(&fence, [7; 32]).unwrap();
            }
            let sql = Connection::open(&path).unwrap();
            // Out-of-band mutations test stale selection independently of normal owner exclusion.
            match corruption {
                "revision" => {
                    sql.execute("UPDATE session_work SET revision=revision+1", [])
                        .unwrap();
                }
                "head" => {
                    sql.execute("DELETE FROM context_checkpoint_heads", [])
                        .unwrap();
                }
                "version" => {
                    sql.execute("UPDATE sessions SET body=json_set(body,'$.version',99)", [])
                        .unwrap();
                }
                _ => {}
            }
            let config = if corruption == "config" {
                [8; 32]
            } else {
                [7; 32]
            };
            if gate == "confirm" {
                assert!(store.confirm_restored(&fence, config).is_err());
            } else {
                let mut command = continuation("go");
                command.configuration_digest = config;
                assert!(store.authorize_continue(&fence, command).is_err());
            }
            assert!(
                store
                    .restore_request(&id, "load")
                    .unwrap()
                    .unwrap()
                    .continuation
                    .is_none()
            );
            store.close_restore(&fence, RestoreClose::Failed).unwrap();
        }
    }
}

#[test]
fn invalid_credentials_and_revision_overflow_never_partially_admit() {
    for invalid in ["token", "overflow"] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (id, mut checkpoint) = capture(&mut store);
        let sql = Connection::open(&path).unwrap();
        if invalid == "overflow" {
            let attempt = store
                .begin_attempt(&id, store.get(&id).unwrap().version)
                .unwrap();
            sql.execute("UPDATE session_work SET revision=9223372036854775807", [])
                .unwrap();
            let value = foundry_store::OpaqueCheckpointInput {
                request_id: "max-revision".into(),
                expected_head: Some(checkpoint.id.clone()),
                cut: store.checkpoint_cut(&attempt).unwrap(),
                format: checkpoint.format.clone(),
                configuration_digest: [7; 32],
                coverage: foundry_store::CoverageManifest {
                    message_ids: vec![],
                    tool_call_ids: vec![],
                },
                payload: vec![1],
                producer_validation: None,
            };
            checkpoint = store
                .finish_with_opaque_checkpoint(
                    &attempt,
                    value,
                    foundry_store::TerminalProposal {
                        lifecycle: foundry_contracts::Lifecycle::Suspended,
                        outcome: None,
                        result: None,
                    },
                )
                .unwrap()
                .checkpoint;
        }
        let fence = begin(&mut store, &id, &checkpoint, "load");
        store.confirm_restored(&fence, [7; 32]).unwrap();
        let before = store.get(&id).unwrap();
        let attempts: i64 = sql
            .query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get(0))
            .unwrap();
        if invalid == "token" {
            sql.execute(
                "UPDATE context_restores SET token=?1",
                ["00000000-0000-0000-0000-000000000000"],
            )
            .unwrap();
        }
        assert!(
            store
                .authorize_continue(&fence, continuation("go"))
                .is_err()
        );
        assert_eq!(store.get(&id).unwrap(), before);
        assert_eq!(
            store.work_revision(&id).unwrap(),
            checkpoint.cut.work_revision
        );
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            attempts
        );
    }
}

#[test]
fn corrupted_receipt_and_oversized_identity_fail_closed_on_read_and_retry() {
    for mutation in ["request_id", "receipt"] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (id, checkpoint) = capture(&mut store);
        let fence = begin(&mut store, &id, &checkpoint, "load");
        store.confirm_restored(&fence, [7; 32]).unwrap();
        store
            .authorize_continue(&fence, continuation("go"))
            .unwrap();
        let sql = Connection::open(&path).unwrap();
        if mutation == "request_id" {
            sql.execute_batch("PRAGMA ignore_check_constraints=ON;")
                .unwrap();
            sql.execute(
                "UPDATE context_restores SET request_id=?1",
                ["x".repeat(131072)],
            )
            .unwrap();
        } else {
            sql.execute(
                "UPDATE context_restores SET admitted_work_revision=admitted_work_revision+1",
                [],
            )
            .unwrap();
            assert!(store.restore_request(&id, "load").is_err());
        }
        assert!(
            store
                .authorize_continue(&fence, continuation("go"))
                .is_err()
        );
    }
}
