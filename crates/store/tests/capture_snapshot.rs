mod support_checkpoint;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_store::{Store, ToolOutcome};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier};
use support_checkpoint::{input, session};
use tempfile::tempdir;

#[test]
fn snapshot_is_sorted_complete_digest_only_and_spans_attempts() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    for (id, text) in [
        ("z", "private first message"),
        ("a", "private second message"),
    ] {
        store.enqueue_message(&fence.session_id, id, text).unwrap();
        store.claim_next_message(&fence).unwrap();
        store.acknowledge_message(&fence, id).unwrap();
    }
    store
        .enqueue_message(&id, "pending", "private pending")
        .unwrap();
    store
        .enqueue_message(&id, "withdrawn", "private withdrawn")
        .unwrap();
    store.withdraw_message(&id, "withdrawn").unwrap();
    store.claim_tool(&fence, "z", "read", [1; 32]).unwrap();
    store
        .finish_tool(&fence, "z", ToolOutcome::Failed, None)
        .unwrap();
    store.claim_tool(&fence, "a", "write", [2; 32]).unwrap();
    store
        .finish_tool(&fence, "a", ToolOutcome::Success, Some([3; 32]))
        .unwrap();
    store
        .append_event(&fence, "event", 0, "text", json!("private event"))
        .unwrap();
    let initial = store.capture_snapshot(&fence).unwrap();
    assert!(initial.head.is_none());
    let mut value = input(&store, &fence, "head");
    value.coverage = initial.coverage;
    let head = store.commit_opaque_checkpoint(&fence, value).unwrap();
    store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    let next = store
        .begin_attempt(&id, store.get(&id).unwrap().version)
        .unwrap();
    store.claim_tool(&next, "b", "read", [4; 32]).unwrap();
    store
        .finish_tool(&next, "b", ToolOutcome::Failed, Some([5; 32]))
        .unwrap();
    let snapshot = store.capture_snapshot(&next).unwrap();
    assert_eq!(snapshot.head, Some(head));
    assert_eq!(snapshot.cut, store.checkpoint_cut(&next).unwrap());
    assert_eq!(snapshot.coverage.message_ids, ["a", "z"]);
    assert_eq!(snapshot.coverage.tool_call_ids, ["a", "b", "z"]);
    assert_eq!(snapshot.messages[0].sequence, 2);
    assert_eq!(snapshot.messages[0].attempt_id, fence.attempt_id);
    assert_eq!(
        snapshot.messages[0].text_digest,
        <[u8; 32]>::from(Sha256::digest(b"private second message"))
    );
    assert_eq!(snapshot.messages[0].text_bytes, 22);
    assert_eq!(
        snapshot.prompt_digest,
        <[u8; 32]>::from(Sha256::digest(b"opaque storage fixture"))
    );
    assert_eq!(snapshot.prompt_bytes, 22);
    assert_eq!(snapshot.tools[1].attempt_id, next.attempt_id);
    assert_eq!(snapshot.tools[1].output_digest, Some([5; 32]));
    assert_eq!(snapshot.tools[2].outcome, ToolOutcome::Failed);
    assert_eq!(snapshot.tools[2].output_digest, None);
    let debug = format!("{snapshot:?}");
    for private in [
        "private second message",
        "private event",
        "opaque storage fixture",
        next.token.as_str(),
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn unsettled_cancelled_expired_and_foreign_owners_never_get_partial_snapshots() {
    for case in 0..6 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let mut fence = store.begin_attempt(&id, 0).unwrap();
        match case {
            0 => {
                store.enqueue_message(&id, "m", "input").unwrap();
                store.claim_next_message(&fence).unwrap();
            }
            1 | 2 => {
                store.claim_tool(&fence, "tool", "write", [1; 32]).unwrap();
                if case == 2 {
                    store
                        .finish_tool(&fence, "tool", ToolOutcome::Uncertain, None)
                        .unwrap();
                }
            }
            3 => {
                store.request_cancel(&id, CancelMode::Graceful).unwrap();
            }
            4 => {
                rusqlite::Connection::open(&path)
                    .unwrap()
                    .execute("UPDATE attempts SET expires_ms=0", [])
                    .unwrap();
            }
            _ => {
                fence.session_id = session(&mut store);
            }
        }
        assert!(store.capture_snapshot(&fence).is_err());
    }
}

#[test]
fn concurrent_message_tool_event_and_head_updates_never_mix_snapshot_generations() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut writer = Store::open(&path).unwrap();
    let id = session(&mut writer);
    let fence = writer.begin_attempt(&id, 0).unwrap();
    let writer_fence = fence.clone();
    let reader = Store::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let started = barrier.clone();
    let writing = std::thread::spawn(move || {
        started.wait();
        for n in 0..25 {
            let message = format!("message-{n}");
            let tool = format!("tool-{n}");
            writer.enqueue_message(&id, &message, "text").unwrap();
            writer.claim_next_message(&writer_fence).unwrap();
            writer.acknowledge_message(&writer_fence, &message).unwrap();
            writer
                .claim_tool(&writer_fence, &tool, "read", [1; 32])
                .unwrap();
            writer
                .finish_tool(&writer_fence, &tool, ToolOutcome::Failed, None)
                .unwrap();
            writer
                .append_event(&writer_fence, &format!("event-{n}"), 0, "text", json!(n))
                .unwrap();
            let snapshot = writer.capture_snapshot(&writer_fence).unwrap();
            let mut value = input(&writer, &writer_fence, &format!("checkpoint-{n}"));
            value.expected_head = snapshot.head.map(|head| head.id);
            value.coverage = snapshot.coverage;
            writer
                .commit_opaque_checkpoint(&writer_fence, value)
                .unwrap();
        }
    });
    barrier.wait();
    for _ in 0..150 {
        if let Ok(snapshot) = reader.capture_snapshot(&fence) {
            assert_eq!(
                snapshot.cut.work_revision,
                1 + 2 * snapshot.messages.len() as u64
                    + 2 * snapshot.tools.len() as u64
                    + snapshot.cut.event_seq
            );
            assert_eq!(snapshot.coverage.message_ids.len(), snapshot.messages.len());
            assert_eq!(snapshot.coverage.tool_call_ids.len(), snapshot.tools.len());
            if let Some(head) = snapshot.head {
                assert!(head.cut.work_revision <= snapshot.cut.work_revision);
            }
        }
    }
    writing.join().unwrap();
    let final_snapshot = reader.capture_snapshot(&fence).unwrap();
    assert_eq!(final_snapshot.messages.len(), 25);
    assert_eq!(final_snapshot.tools.len(), 25);
    assert_eq!(final_snapshot.cut.event_seq, 25);
    assert_eq!(final_snapshot.head.unwrap().cut, final_snapshot.cut);
}
