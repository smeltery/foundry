use foundry_contracts::{CreateSession, MAX_FRAME_BYTES, MAX_PROMPT_BYTES};
use foundry_store::{AttemptFence, MAX_CAPTURE_EVIDENCE_ENTRIES, Store};
use rusqlite::Connection;
use tempfile::tempdir;

fn fixture(store: &mut Store) -> AttemptFence {
    let session = store
        .create(CreateSession {
            prompt: "prompt".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    store.begin_attempt(&session.id, 0).unwrap()
}
fn deliver(store: &mut Store, fence: &AttemptFence, id: &str) {
    store
        .enqueue_message(&fence.session_id, id, "text")
        .unwrap();
    store.claim_next_message(fence).unwrap();
    store.acknowledge_message(fence, id).unwrap();
}

#[test]
fn evidence_count_includes_prompt_and_never_returns_a_prefix() {
    let mut store = Store::open(":memory:").unwrap();
    let fence = fixture(&mut store);
    for n in 0..MAX_CAPTURE_EVIDENCE_ENTRIES - 1 {
        deliver(&mut store, &fence, &format!("{n:04}"));
    }
    assert_eq!(
        store.capture_snapshot(&fence).unwrap().messages.len(),
        MAX_CAPTURE_EVIDENCE_ENTRIES - 1
    );
    deliver(&mut store, &fence, "overflow");
    assert!(
        store
            .capture_snapshot(&fence)
            .unwrap_err()
            .to_string()
            .contains("count")
    );
    assert_eq!(
        store.messages(&fence.session_id, 0, 2048).unwrap().len(),
        1024
    );
}

#[test]
fn raw_identity_and_exact_json_escape_budgets_are_separate() {
    for escaped in [false, true] {
        let mut store = Store::open(":memory:").unwrap();
        let fence = fixture(&mut store);
        let count = if escaped { 256 } else { 399 };
        let suffix = if escaped {
            "\"".repeat(124)
        } else {
            "x".repeat(124)
        };
        for n in 0..count {
            deliver(&mut store, &fence, &format!("{n:04}{suffix}"));
        }
        let snapshot = store.capture_snapshot(&fence).unwrap();
        let encoded =
            serde_json::json!({"message_ids":snapshot.coverage.message_ids,"tool_call_ids":[]})
                .to_string();
        assert!(encoded.len() <= 65536);
        deliver(&mut store, &fence, &format!("{count:04}{suffix}"));
        let error = store.capture_snapshot(&fence).unwrap_err().to_string();
        assert!(
            error.contains(if escaped {
                "encoded coverage"
            } else {
                "identity byte"
            }),
            "{error}"
        );
    }
}

#[test]
fn oversized_or_malformed_stored_fields_fail_before_evidence_copy() {
    for field in [
        "prompt",
        "text",
        "message_id",
        "event",
        "kind",
        "attempt",
        "digest",
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let fence = fixture(&mut store);
        deliver(&mut store, &fence, "m");
        store
            .append_event(&fence, "e", 0, "text", serde_json::json!("small"))
            .unwrap();
        let sql = Connection::open(&path).unwrap();
        match field {
            "prompt" => {
                sql.execute(
                    "UPDATE sessions SET body=json_set(body,'$.prompt',?1)",
                    ["x".repeat(MAX_PROMPT_BYTES + 1)],
                )
                .unwrap();
            }
            "text" => {
                sql.execute(
                    "UPDATE messages SET text=?1",
                    ["x".repeat(MAX_PROMPT_BYTES + 1)],
                )
                .unwrap();
            }
            "message_id" => {
                sql.execute("UPDATE messages SET id=?1", ["x".repeat(129)])
                    .unwrap();
            }
            "event" => {
                sql.execute(
                    "UPDATE events SET data=?1",
                    ["x".repeat(MAX_FRAME_BYTES + 1)],
                )
                .unwrap();
            }
            "kind" => {
                sql.execute("UPDATE events SET kind='bad\nkind'", [])
                    .unwrap();
            }
            "attempt" => {
                sql.execute("UPDATE messages SET attempt_id=NULL", [])
                    .unwrap();
            }
            _ => {
                store.claim_tool(&fence, "tool", "read", [1; 32]).unwrap();
                store
                    .finish_tool(&fence, "tool", foundry_store::ToolOutcome::Failed, None)
                    .unwrap();
                sql.execute_batch("PRAGMA ignore_check_constraints=ON;")
                    .unwrap();
                sql.execute("UPDATE tool_calls SET input_digest=zeroblob(33)", [])
                    .unwrap();
            }
        }
        assert!(store.capture_snapshot(&fence).is_err(), "{field}");
    }
}

#[test]
fn prompt_and_event_exact_limits_succeed_without_copying_unrelated_session_result() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let fence = fixture(&mut store);
    let sql = Connection::open(&path).unwrap();
    sql.execute(
        "UPDATE sessions SET body=json_set(body,'$.prompt',?1,'$.result',?2)",
        rusqlite::params![
            "x".repeat(MAX_PROMPT_BYTES),
            "not snapshot evidence".repeat(100000)
        ],
    )
    .unwrap();
    store
        .append_event(
            &fence,
            "event",
            0,
            "text",
            serde_json::json!("x".repeat(MAX_FRAME_BYTES - 2)),
        )
        .unwrap();
    let snapshot = store.capture_snapshot(&fence).unwrap();
    assert_eq!(snapshot.prompt_bytes, MAX_PROMPT_BYTES as u64);
    assert_eq!(snapshot.cut.event_seq, 1);
    assert_eq!(snapshot.cut, store.checkpoint_cut(&fence).unwrap());
    assert!(!format!("{snapshot:?}").contains("not snapshot evidence"));
}
