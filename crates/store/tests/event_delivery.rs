use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_store::{AttemptFence, DeliveryCursor, EventRead, Store, StoreError, transcript};
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;

const ATTEMPT_SPAN: u64 = 1 << 32;

fn create(store: &mut Store) -> String {
    store
        .create(CreateSession {
            prompt: "deliver".into(),
            directory: "/workspace/sample".into(),
            model: None,
            idempotency_key: None,
            ..Default::default()
        })
        .unwrap()
        .id
}

fn next_attempt(store: &mut Store, fence: AttemptFence) -> AttemptFence {
    let session = store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    store.begin_attempt(&session.id, session.version).unwrap()
}

fn read(after_seq: DeliveryCursor) -> EventRead {
    EventRead {
        after_seq,
        ..Default::default()
    }
}

fn delivery(store: &mut Store, id: &str, after_seq: DeliveryCursor) -> Vec<u64> {
    let page = store.deliver_events(id, &read(after_seq)).unwrap();
    page.events.iter().map(|event| event.delivery_seq).collect()
}

fn stamps(path: &Path) -> Vec<(i64, Option<i64>, Option<i64>)> {
    let db = Connection::open(path).unwrap();
    let mut rows = db
        .prepare("SELECT sequence,attempt_ordinal,position FROM events ORDER BY sequence")
        .unwrap();
    rows.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn invalid(error: StoreError) -> bool {
    matches!(error, StoreError::Contract(ContractError::Invalid(_)))
}

#[test]
fn later_attempt_is_offset_and_transcript_keeps_highest_copy_by_position() {
    let mut store = Store::open(":memory:").unwrap();
    let id = create(&mut store);
    let first = store.begin_attempt(&id, 0).unwrap();
    store
        .append_event(&first, "text-1", 0, "text", json!("Par"))
        .unwrap();
    store
        .append_event(&first, "tool-1", 0, "tool", json!({}))
        .unwrap();
    store
        .append_event(&first, "text-1", 1, "text", json!("Part"))
        .unwrap();
    let page = store.deliver_events(&id, &EventRead::default()).unwrap();
    let seen: Vec<_> = page
        .events
        .iter()
        .map(|e| {
            (
                e.delivery_seq,
                e.attempt_ordinal,
                e.position,
                e.event.sequence,
            )
        })
        .collect();
    assert_eq!(seen, [(1, 1, 1, 1), (2, 1, 2, 2), (3, 1, 1, 3)]);
    assert_eq!(page.next_seq, DeliveryCursor::After(3));
    let display: Vec<_> = transcript(&page.events)
        .iter()
        .map(|e| (e.event.id.as_str(), e.event.revision))
        .collect();
    assert_eq!(display, [("text-1", 1), ("tool-1", 0)]);

    let second = next_attempt(&mut store, first);
    let event = store
        .append_event(&second, "text-2", 0, "text", json!("x"))
        .unwrap();
    assert_eq!(event.sequence, 4, "the stored sequence is not rescaled");
    let later = store.deliver_events(&id, &read(page.next_seq)).unwrap();
    assert_eq!(later.events.len(), 1);
    assert_eq!(later.events[0].delivery_seq, ATTEMPT_SPAN + 4);
    assert_eq!(later.events[0].attempt_ordinal, 2);
    assert_eq!(later.next_seq, DeliveryCursor::After(ATTEMPT_SPAN + 4));
    let drained = store.deliver_events(&id, &read(later.next_seq)).unwrap();
    assert!(drained.events.is_empty());
    assert_eq!(
        drained.next_seq, later.next_seq,
        "an empty page keeps the cursor"
    );
    assert_eq!(drained.session.lifecycle, Lifecycle::Running);
    // Cursors inside the gap between attempt spaces still resume correctly.
    assert_eq!(
        delivery(&mut store, &id, DeliveryCursor::After(ATTEMPT_SPAN - 1)),
        [ATTEMPT_SPAN + 4]
    );
}

#[test]
fn page_wait_and_cursor_bounds_are_enforced() {
    let mut store = Store::open(":memory:").unwrap();
    let id = create(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    for n in 0..201 {
        store
            .append_event(&fence, &format!("e{n}"), 0, "text", json!(n))
            .unwrap();
    }
    let page = store.deliver_events(&id, &EventRead::default()).unwrap();
    assert_eq!(page.events.len(), 200, "default page");
    assert_eq!(page.next_seq, DeliveryCursor::After(200));
    assert_eq!(delivery(&mut store, &id, page.next_seq), [201]);
    for (limit, accepted) in [(0, false), (1, true), (1000, true), (1001, false)] {
        let request = EventRead {
            limit: Some(limit),
            ..Default::default()
        };
        let result = store.deliver_events(&id, &request);
        match accepted {
            true => assert_eq!(result.unwrap().events.len(), limit.min(201) as usize),
            false => assert!(invalid(result.unwrap_err()), "limit {limit}"),
        }
    }
    for (wait_ms, accepted) in [(0, true), (20_000, true), (20_001, false)] {
        let request = EventRead {
            wait_ms: Some(wait_ms),
            ..Default::default()
        };
        assert_eq!(request.wait().is_ok(), accepted, "wait {wait_ms}");
        assert_eq!(store.deliver_events(&id, &request).is_ok(), accepted);
    }
    assert_eq!(
        DeliveryCursor::from_wire(-1).unwrap(),
        DeliveryCursor::Start
    );
    assert_eq!(DeliveryCursor::default(), DeliveryCursor::Start);
    let max = DeliveryCursor::from_wire(u64::MAX.into()).unwrap();
    assert_eq!(max, DeliveryCursor::After(u64::MAX));
    assert_eq!(max.to_wire(), i128::from(u64::MAX));
    assert_eq!(DeliveryCursor::Start.to_string(), "-1");
    for value in [-2, i128::from(u64::MAX) + 1] {
        assert!(invalid(DeliveryCursor::from_wire(value).unwrap_err()));
    }
    assert!(
        store
            .deliver_events("missing", &EventRead::default())
            .is_err()
    );
}

#[test]
fn delivery_values_beyond_signed_range_are_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = create(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    store
        .append_event(&fence, "e", 0, "text", json!(1))
        .unwrap();
    let db = Connection::open(&path).unwrap();
    let cases = [
        ((1_i64 << 31) + 1, (1_u64 << 63) + 1),
        (1_i64 << 32, ((ATTEMPT_SPAN - 1) << 32) + 1),
    ];
    for (ordinal, expected) in cases {
        db.execute("UPDATE events SET attempt_ordinal=?1", [ordinal])
            .unwrap();
        assert!(expected > i64::MAX as u64);
        assert_eq!(delivery(&mut store, &id, DeliveryCursor::Start), [expected]);
        let page = store.deliver_events(&id, &EventRead::default()).unwrap();
        assert_eq!(page.events[0].attempt_ordinal, ordinal as u64);
        assert_eq!(page.next_seq, DeliveryCursor::After(expected));
        assert_eq!(
            delivery(&mut store, &id, DeliveryCursor::After(expected - 1)),
            [expected]
        );
        assert!(delivery(&mut store, &id, DeliveryCursor::After(expected)).is_empty());
        assert!(delivery(&mut store, &id, DeliveryCursor::After(u64::MAX)).is_empty());
    }
    db.execute("UPDATE events SET attempt_ordinal=?1", [(1_i64 << 32) + 1])
        .unwrap();
    assert!(store.deliver_events(&id, &EventRead::default()).is_err());
}

#[test]
fn unstamped_rows_are_backfilled_deterministically_without_touching_the_cut() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = create(&mut store);
    let first = store.begin_attempt(&id, 0).unwrap();
    store
        .append_event(&first, "a", 0, "text", json!("a"))
        .unwrap();
    store
        .append_event(&first, "b", 0, "text", json!("b"))
        .unwrap();
    let second = next_attempt(&mut store, first);
    store
        .append_event(&second, "c", 0, "text", json!("c"))
        .unwrap();
    store
        .append_event(&second, "c", 1, "text", json!("cc"))
        .unwrap();
    let stamped = stamps(&path);
    assert_eq!(
        stamped,
        [
            (1, Some(1), Some(1)),
            (2, Some(1), Some(2)),
            (3, Some(2), Some(3)),
            (4, Some(2), Some(3))
        ]
    );
    let cut = store.checkpoint_cut(&second).unwrap();
    let db = Connection::open(&path).unwrap();
    let clear = "UPDATE events SET attempt_ordinal=NULL, position=NULL";

    // A delivery read stamps rows written before v12 by the same rule.
    db.execute(clear, []).unwrap();
    let expected = [1, 2, ATTEMPT_SPAN + 3, ATTEMPT_SPAN + 4];
    assert_eq!(delivery(&mut store, &id, DeliveryCursor::Start), expected);
    assert_eq!(stamps(&path), stamped);
    assert_eq!(store.checkpoint_cut(&second).unwrap(), cut);

    // So does the next write, including partially stamped and stored values.
    db.execute(clear, []).unwrap();
    db.execute("UPDATE events SET position=2 WHERE sequence=1", [])
        .unwrap();
    store
        .append_event(&second, "c", 2, "text", json!("ccc"))
        .unwrap();
    let mut rewritten = stamped.clone();
    rewritten[0].2 = Some(2);
    rewritten.push((5, Some(2), Some(3)));
    assert_eq!(stamps(&path), rewritten);

    // A stored ordinal that decreases is reported rather than reordered.
    db.execute("UPDATE events SET position=NULL WHERE sequence=5", [])
        .unwrap();
    db.execute("UPDATE events SET attempt_ordinal=1 WHERE sequence=5", [])
        .unwrap();
    assert!(store.deliver_events(&id, &EventRead::default()).is_err());
}

#[test]
fn v11_fixture_rows_are_stamped_on_first_delivery_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let fixture = include_str!("fixtures/store-v11.sql");
    Connection::open(&path)
        .unwrap()
        .execute_batch(fixture)
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    let id = "54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc";
    let page = store.deliver_events(id, &EventRead::default()).unwrap();
    let seen: Vec<_> = page
        .events
        .iter()
        .map(|e| {
            (
                e.delivery_seq,
                e.position,
                e.event.id.as_str(),
                e.event.revision,
            )
        })
        .collect();
    assert_eq!(seen, [(1, 1, "e1", 0), (2, 2, "e2", 0), (3, 1, "e1", 1)]);
    let untouched = stamps(&path)
        .into_iter()
        .filter(|(_, ordinal, _)| ordinal.is_none())
        .count();
    assert_eq!(untouched, 1, "other sessions stay unstamped until read");
}
