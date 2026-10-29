use super::*;
#[test]
fn read_unread_and_held_deadlines_expire_truthful_reference_kinds() {
    for (label, reference, deadline, kind) in [
        (
            "read",
            RetentionReference::Read { at_ms: 20 },
            20 + RETAIN_RELEASED_MS,
            ExpiredReference::ReadAttachments,
        ),
        (
            "unread",
            RetentionReference::Unread,
            10 + RETAIN_UNREAD_MS,
            ExpiredReference::UnreadAttachments,
        ),
        (
            "held",
            RetentionReference::Held,
            10 + RETAIN_UNREAD_MS,
            ExpiredReference::HeldMessage,
        ),
    ] {
        let root = Root::new();
        let mut store = AttachmentStore::open(&root.0).unwrap();
        let owner = key(label);
        let content = [file("receipt.txt", "retained bytes")];
        let saved = store.persist(&owner, &content, 10).unwrap();
        assert_eq!(
            store.age_out(&owner, reference, deadline - 1).unwrap(),
            ExpiryOutcome::NotDue {
                expires_ms: deadline
            }
        );
        assert!(store.recover(&owner).is_ok());
        assert_eq!(
            store.age_out(&owner, reference, deadline).unwrap(),
            ExpiryOutcome::Expired {
                reference: kind,
                cleanup_error: None
            }
        );
        assert!(!PathBuf::from(saved[0].path.as_ref().unwrap()).exists());
        assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Expired);
        assert_eq!(
            store.load_payload(&owner).unwrap_err(),
            AttachmentError::Expired
        );
        assert_eq!(
            store.persist(&owner, &content, deadline).unwrap_err(),
            AttachmentError::Expired
        );
        assert_eq!(
            store
                .persist(&owner, &[file("receipt.txt", "different")], deadline)
                .unwrap_err(),
            AttachmentError::Conflict
        );
        assert_eq!(store.usage().unwrap().bytes, 0);
        drop(store);
        let mut store = AttachmentStore::open(&root.0).unwrap();
        assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Expired);
        assert_eq!(
            store.age_out(&owner, reference, deadline + 1).unwrap(),
            ExpiryOutcome::Expired {
                reference: kind,
                cleanup_error: None
            }
        );
        store.forget_expired(&owner).unwrap();
        assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Missing);
    }
}
#[test]
fn expiration_revokes_availability_even_when_local_cleanup_conflicts() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("interrupted-expiry");
    let saved = store.persist(&owner, &[file("a.txt", "known")], 0).unwrap();
    let extra = PathBuf::from(saved[0].path.as_ref().unwrap())
        .parent()
        .unwrap()
        .join("unexpected.txt");
    fs::write(&extra, "preserve").unwrap();
    assert_eq!(
        store
            .age_out(&owner, RetentionReference::Held, RETAIN_UNREAD_MS)
            .unwrap(),
        ExpiryOutcome::Expired {
            reference: ExpiredReference::HeldMessage,
            cleanup_error: Some(AttachmentError::Conflict)
        }
    );
    assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Expired);
    assert!(store.forget_expired(&owner).is_err());
    assert_eq!(fs::read_to_string(&extra).unwrap(), "preserve");
    drop(store);
    fs::remove_file(extra).unwrap();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert_eq!(
        store
            .age_out(&owner, RetentionReference::Unread, RETAIN_UNREAD_MS + 1)
            .unwrap(),
        ExpiryOutcome::Expired {
            reference: ExpiredReference::HeldMessage,
            cleanup_error: None
        }
    );
}
