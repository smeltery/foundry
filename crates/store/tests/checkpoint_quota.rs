mod support_checkpoint;
use foundry_store::{MAX_CHECKPOINT_BYTES, Store};
use support_checkpoint::{input, session};
use tempfile::tempdir;

#[test]
fn production_session_quota_rolls_back_publication_and_retry_is_free() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("quota.db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let mut head = None;
    for n in 0..255 {
        store.heartbeat(&fence).unwrap();
        let mut value = input(&store, &fence, &format!("request-{n}"));
        value.payload = vec![0; MAX_CHECKPOINT_BYTES];
        value.expected_head = head;
        head = Some(store.commit_opaque_checkpoint(&fence, value).unwrap().id);
    }
    let before = store.checkpoint_head(&id).unwrap().unwrap();
    let mut over = input(&store, &fence, "over-quota");
    over.payload = vec![0; MAX_CHECKPOINT_BYTES];
    over.expected_head = head;
    assert!(
        store
            .commit_opaque_checkpoint(&fence, over)
            .unwrap_err()
            .to_string()
            .contains("quota")
    );
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(before.clone()));
    assert!(
        store
            .checkpoint_request(&id, "over-quota")
            .unwrap()
            .is_none()
    );
    let mut retry = input(&store, &fence, "request-254");
    retry.payload = vec![0; MAX_CHECKPOINT_BYTES];
    retry.expected_head = before.parent_id.clone();
    assert_eq!(
        store.commit_opaque_checkpoint(&fence, retry).unwrap(),
        before
    );
}
