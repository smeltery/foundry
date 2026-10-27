use foundry_contracts::CreateSession;
use foundry_store::{Store, ToolOutcome};
use sha2::{Digest, Sha256};

// Frozen pre-snapshot encoding: domain and every field have a u64 big-endian length.
fn legacy(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for field in std::iter::once(domain).chain(fields.iter().copied()) {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}

#[test]
fn cut_preserves_legacy_field_order_lengths_domains_and_optional_result_tags() {
    let mut store = Store::open(":memory:").unwrap();
    let id = store
        .create(CreateSession {
            prompt: "prompt雪".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
        .id;
    let fence = store.begin_attempt(&id, 0).unwrap();
    store
        .append_event(&fence, "event", 0, "text", serde_json::json!("payload"))
        .unwrap();
    for (id, text) in [("z", "first雪"), ("a", "second")] {
        store.enqueue_message(&fence.session_id, id, text).unwrap();
        store.claim_next_message(&fence).unwrap();
        store.acknowledge_message(&fence, id).unwrap();
    }
    store.claim_tool(&fence, "z", "read", [2; 32]).unwrap();
    store
        .finish_tool(&fence, "z", ToolOutcome::Success, Some([3; 32]))
        .unwrap();
    store.claim_tool(&fence, "a", "write", [1; 32]).unwrap();
    store
        .finish_tool(&fence, "a", ToolOutcome::Failed, None)
        .unwrap();
    let cut = store.checkpoint_cut(&fence).unwrap();
    let attempt = fence.attempt_id.as_bytes();
    assert_eq!(
        cut.event_digest,
        legacy(
            b"foundry.checkpoint.events.v1",
            &[
                &1i64.to_be_bytes(),
                b"event",
                &0i64.to_be_bytes(),
                attempt,
                b"text",
                br#""payload""#,
            ]
        )
    );
    assert_eq!(
        cut.input_digest,
        legacy(
            b"foundry.checkpoint.inputs.v1",
            &[
                "prompt雪".as_bytes(),
                b"z",
                &1i64.to_be_bytes(),
                "first雪".as_bytes(),
                attempt,
                b"a",
                &2i64.to_be_bytes(),
                b"second",
                attempt,
            ]
        )
    );
    assert_eq!(
        cut.tool_digest,
        legacy(
            b"foundry.checkpoint.tools.v1",
            &[
                b"a",
                attempt,
                b"write",
                &[1; 32],
                b"failed",
                &[0],
                b"z",
                attempt,
                b"read",
                &[2; 32],
                b"success",
                &[1],
                &[3; 32],
            ]
        )
    );
    assert_eq!(cut.work_revision, 10);
    assert_eq!(cut.event_seq, 1);
    assert_eq!(store.capture_snapshot(&fence).unwrap().cut, cut);
}

#[test]
fn bundled_sqlite_decodes_escaped_prompt_bytes_without_changing_frozen_hashes() {
    let cases = [
        (
            r#""A\nB""#,
            "A\nB",
            "520030d2783e9d25932b3e3ea7946baa41845c508fe4325456c47818e3154520",
            "23519a43c66b4c342f25b32e09797ec5f3fc0be388cd8243fb3449afbdce4013",
        ),
        (
            r#""A\\B""#,
            "A\\B",
            "7e887cd7bd73a6e7cec280c1c34f26fee79c0227aa587a9286e265f986dc9295",
            "edc0916552f4dbb224da7a63ebb17d9c54453ca43548cf109ca26389c39fbf0e",
        ),
        (
            r#""A\u0000B""#,
            "A\0B",
            "9dc85b265a5946d224c542a61123aa70eaad38b50bce2d756c86345c458bbad5",
            "76fe3925c7167317f2df68454339f5ec3650e4062178b4f2be219b105a507907",
        ),
        (
            r#""A\ud83e\uddea\ud801\udc37B""#,
            "A🧪𐐷B",
            "84b633deec504e1076905265e22ec12135fdddf04d5dd03af96afc5d8f8f46d7",
            "79b81e07e27c6ca8dece6e60c8fa50cc141a8c95cfa01fdc77acaad59be544c3",
        ),
    ];
    let hex = |digest: [u8; 32]| {
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    for (encoded, decoded, input_hash, prompt_hash) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = store
            .create(CreateSession {
                prompt: "placeholder".into(),
                directory: "/tmp".into(),
                model: None,
                idempotency_key: None,
            })
            .unwrap()
            .id;
        let fence = store.begin_attempt(&id, 0).unwrap();
        // Preserve literal JSON escapes, including surrogate pairs, for SQLite to decode.
        let sql = rusqlite::Connection::open(&path).unwrap();
        sql.execute(
            "UPDATE sessions SET body=replace(body,'\"prompt\":\"placeholder\"',?1) WHERE id=?2",
            rusqlite::params![format!("\"prompt\":{encoded}"), id],
        )
        .unwrap();
        let body: String = sql
            .query_row("SELECT body FROM sessions WHERE id=?1", [&id], |r| r.get(0))
            .unwrap();
        assert!(body.contains(encoded));
        assert_eq!(store.get(&id).unwrap().prompt, decoded); // Independent serde decoding.
        let cut = store.checkpoint_cut(&fence).unwrap();
        let snapshot = store.capture_snapshot(&fence).unwrap();
        assert_eq!(hex(cut.input_digest), input_hash, "{encoded}");
        assert_eq!(snapshot.cut, cut);
        assert_eq!(hex(snapshot.prompt_digest), prompt_hash, "{encoded}");
        assert_eq!(snapshot.prompt_bytes, decoded.len() as u64);
    }
}
