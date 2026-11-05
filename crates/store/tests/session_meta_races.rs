//! Concurrent metadata writers on separate connections to one database.
use foundry_contracts::{CreateSession, Lifecycle, Outcome};
use foundry_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::{
    sync::{Arc, Barrier},
    thread,
};
use tempfile::tempdir;

fn request(prompt: &str) -> CreateSession {
    CreateSession {
        prompt: prompt.into(),
        directory: "/workspace/project".into(),
        ..Default::default()
    }
}

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn concurrent_tag_patches_from_separate_connections_never_lose_a_tag() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let id = Store::open(&path)
        .unwrap()
        .create(request("race"))
        .unwrap()
        .id;
    let writers = 6;
    let barrier = Arc::new(Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|writer| {
            let (path, id, barrier) = (path.clone(), id.clone(), barrier.clone());
            thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                for n in 0..10 {
                    let ours = format!("w{writer}-{n}");
                    let stored = store
                        .append_tags(&id, &[ours.clone(), "shared".into()])
                        .unwrap();
                    assert!(stored.contains(&ours));
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let store = Store::open(&path).unwrap();
    let stored = store.session_tags(&id).unwrap();
    assert_eq!(stored.len(), writers * 10 + 1);
    assert_eq!(stored.iter().filter(|tag| *tag == "shared").count(), 1);
    for writer in 0..writers {
        // Each writer's own appends keep their relative order.
        let own: Vec<_> = stored
            .iter()
            .filter(|tag| tag.starts_with(&format!("w{writer}-")))
            .cloned()
            .collect();
        assert_eq!(
            own,
            (0..10)
                .map(|n| format!("w{writer}-{n}"))
                .collect::<Vec<_>>()
        );
    }
    let db = Connection::open(&path).unwrap();
    let positions: Vec<i64> = db
        .prepare("SELECT position FROM session_tags WHERE session_id=?1 ORDER BY position")
        .unwrap()
        .query_map([&id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(positions, (0..stored.len() as i64).collect::<Vec<_>>());
}

#[test]
fn concurrent_replace_and_append_leave_one_serial_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let id = Store::open(&path)
        .unwrap()
        .create(request("race"))
        .unwrap()
        .id;
    let barrier = Arc::new(Barrier::new(2));
    let spawn = |replace: bool| {
        let (path, id, barrier) = (path.clone(), id.clone(), barrier.clone());
        thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            for n in 0..40 {
                if replace {
                    store.replace_tags(&id, &tags(&["base"])).unwrap();
                } else {
                    store.append_tags(&id, &[format!("a{n}")]).unwrap();
                }
            }
        })
    };
    let handles = [spawn(true), spawn(false)];
    for handle in handles {
        handle.join().unwrap();
    }
    let stored = Store::open(&path).unwrap().session_tags(&id).unwrap();
    // Some replace ran last or was followed only by appends: the list is
    // "base" and then appends in their submission order, or appends alone.
    let appended: Vec<_> = stored.iter().filter(|tag| *tag != "base").collect();
    assert!(appended.windows(2).all(|pair| {
        let n = |tag: &str| tag[1..].parse::<u32>().unwrap();
        n(pair[0]) < n(pair[1])
    }));
    if stored.contains(&"base".to_owned()) {
        assert_eq!(stored[0], "base");
    }
    assert!(stored.contains(&"a39".to_owned()) || stored == ["base"]);
}

#[test]
fn concurrent_archive_rename_and_lifecycle_writes_all_land() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut owner = Store::open(&path).unwrap();
    let id = owner.create(request("race")).unwrap().id;
    let barrier = Arc::new(Barrier::new(3));
    let spawn = |rename: bool| {
        let (path, id, barrier) = (path.clone(), id.clone(), barrier.clone());
        thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            for n in 0..30 {
                if rename {
                    store
                        .rename_session(&id, Some(&format!("title {n}")))
                        .unwrap();
                } else {
                    store.set_archived(&id, n % 2 == 0).unwrap();
                }
            }
        })
    };
    let handles = [spawn(true), spawn(false)];
    barrier.wait();
    // Every admission and terminal rewrites the body while metadata writers run;
    // a body read outside its write transaction would restore a stale title.
    for n in 0..30 {
        let version = owner.get(&id).unwrap().version;
        let fence = owner.begin_attempt(&id, version).unwrap();
        owner
            .append_event(&fence, &format!("e{n}"), 0, "text", json!(n))
            .unwrap();
        owner
            .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
            .unwrap();
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let session = owner.get(&id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Exit);
    assert_eq!(session.version, 60);
    assert_eq!(session.metadata.title.as_deref(), Some("title 29"));
    assert!(!session.metadata.archived);
    assert_eq!(owner.events(&id, 0, 100).unwrap().len(), 30);
    let db = Connection::open(&path).unwrap();
    let columns: (Option<String>, i64) = db
        .query_row(
            "SELECT title,archived FROM sessions WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(columns, (Some("title 29".into()), 0));
}
