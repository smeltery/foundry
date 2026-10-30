use super::*;
#[test]
fn durable_sender_rate_and_kept_quota_fail_without_pruning_pending_references() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let files = [
        file("a.txt", &"x".repeat(MAX_FILE_BYTES)),
        file("b.txt", &"y".repeat(MAX_FILE_BYTES)),
    ];
    for i in 0..40 {
        store
            .persist(&key(&format!("rate-{i}")), &files, 0)
            .unwrap();
    }
    assert_eq!(
        store.persist(&key("rate-over"), &files, 0).unwrap_err(),
        AttachmentError::Quota
    );
    assert_eq!(
        store
            .persist(&key("exact-hour"), &files, 3_600_000)
            .unwrap_err(),
        AttachmentError::Quota
    );
    drop(store);
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert_eq!(
        store
            .persist(&key("restart-rate-over"), &files, 0)
            .unwrap_err(),
        AttachmentError::Quota
    );
    for i in 40..50 {
        store
            .persist(&key(&format!("kept-{i}")), &files, 3_600_001)
            .unwrap();
    }
    assert_eq!(
        store
            .persist(&key("kept-over"), &files, 3_600_001)
            .unwrap_err(),
        AttachmentError::Quota
    );
    assert_eq!(store.usage().unwrap().bytes, MAX_SENDER_BYTES);
    assert_eq!(
        store
            .cleanup(&key("rate-0"), RETAIN_RELEASED_MS * 8)
            .unwrap_err(),
        AttachmentError::InUse
    );
    assert!(store.recover(&key("rate-0")).is_ok());
    // Exact retries are admitted even while full and do not debit the hourly budget twice.
    assert!(store.persist(&key("rate-0"), &files, 3_600_001).is_ok());
}
#[test]
fn corrupt_payload_and_owner_binding_fail_instead_of_becoming_empty() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("corrupt");
    store.persist(&owner, &[file("a.txt", "body")], 0).unwrap();
    let sql = rusqlite::Connection::open(root.0.join("attachments.sqlite3")).unwrap();
    sql.execute("UPDATE batches SET sender='different'", [])
        .unwrap();
    assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Corrupt);
    sql.execute("UPDATE batches SET sender='sender',payload=x'ff'", [])
        .unwrap();
    assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Corrupt);
}

#[test]
fn global_quota_requires_explicit_reference_release_and_cleanup_to_free_capacity() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let files = [
        file("a.txt", &"a".repeat(MAX_FILE_BYTES)),
        file("b.txt", &"b".repeat(MAX_FILE_BYTES)),
    ];
    for sender in 0..5 {
        for message in 0..40 {
            let owner = AttachmentKey::new(
                &format!("person-{sender}"),
                "reader",
                &format!("item-{message}"),
            )
            .unwrap();
            store.persist(&owner, &files, 0).unwrap();
        }
    }
    assert_eq!(store.usage().unwrap().bytes, MAX_KEPT_BYTES);
    let another = AttachmentKey::new("other-person", "reader", "new").unwrap();
    assert_eq!(
        store
            .persist(&another, &[file("one.txt", "x")], 0)
            .unwrap_err(),
        AttachmentError::Quota
    );
    let old = AttachmentKey::new("person-0", "reader", "item-0").unwrap();
    store.release(&old, 0).unwrap();
    store.cleanup(&old, RETAIN_RELEASED_MS).unwrap();
    assert!(store.persist(&another, &files, RETAIN_RELEASED_MS).is_ok());
    assert_eq!(store.usage().unwrap().bytes, MAX_KEPT_BYTES);
}

#[test]
fn empty_reserved_batches_cannot_bypass_metadata_capacity() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    // Valid durable reservations before materialization, as after an interrupted persist.
    let mut sql = rusqlite::Connection::open(root.0.join("attachments.sqlite3")).unwrap();
    let tx = sql.transaction().unwrap();
    for n in 0..MAX_BATCHES {
        let owner = key(&format!("reserved-{n}"));
        tx.execute("INSERT INTO batches(id,sender,recipient,message,payload,bytes,accepted,state,fingerprint,names) VALUES(?1,?2,?3,?4,?5,0,0,0,?6,?5)", rusqlite::params![directory(&owner),owner.sender,owner.recipient,owner.message,b"[]".as_slice(),blake3::hash(b"[]").as_bytes().as_slice()]).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(
        store.persist(&key("extra"), &[], 0).unwrap_err(),
        AttachmentError::Quota
    );
    assert_eq!(
        store.usage().unwrap(),
        Usage {
            batches: MAX_BATCHES,
            bytes: 0
        }
    );
    assert!(store.recover(&key("reserved-0")).is_ok());
}

#[test]
fn unrelated_private_database_is_not_reconfigured_as_attachment_storage() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let path = root.0.join("attachments.sqlite3");
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("PRAGMA application_id=123; CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES('preserved'); PRAGMA journal_mode=WAL;").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(sql);
    assert!(AttachmentStore::open(&root.0).is_err());
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        123
    );
    assert_eq!(
        sql.query_row("SELECT value FROM unrelated", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "preserved"
    );
    assert_eq!(
        sql.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
}
