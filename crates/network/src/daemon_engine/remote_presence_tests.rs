use super::*;
use rusqlite::Connection;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    root: PathBuf,
    member: RemoteMember,
}
impl Fixture {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "presence-check-{:032x}",
            u128::from_ne_bytes(random)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
            member: RemoteMember {
                peer: iroh::SecretKey::generate().public(),
                person: "river".into(),
                machine: "desk".into(),
            },
        }
    }
    fn engine(&self) -> Engine {
        Engine::open_with_clock(
            &self.root,
            registry::FolderPolicy::new(vec![(self.root.clone(), false)]).unwrap(),
            TrustedIdentity {
                person: "local".into(),
                machine: "station".into(),
            },
            || Ok(100),
        )
        .unwrap()
    }
    fn ready(&self) -> Engine {
        let mut engine = self.engine();
        engine
            .sync_remote_members("current", 1, std::slice::from_ref(&self.member))
            .unwrap();
        engine
    }
    fn observer(&self) -> Connection {
        Connection::open(self.root.join("mail.sqlite3")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn advertised() -> wire::Presence {
    wire::Presence {
        sessions: vec![wire::SessionPresence {
            address: "ignored".into(),
            name: "writer".into(),
            person: "asserted".into(),
            machine: "asserted".into(),
            agent: "client".into(),
            cwd: String::new(),
            repo: None,
            branch: None,
            status: "available".into(),
            since: Some(1),
            doing: None,
            tier: wire::DeliveryTier::Pull,
            unread: 0,
            is_me: true,
        }],
        known: vec!["saved".into()],
        kept: vec!["retained".into()],
        features: vec!["text".into()],
    }
}
fn version(observer: &Connection) -> i64 {
    observer
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap()
}
fn publish(engine: &mut Engine, fixture: &Fixture, generation: u64, value: &wire::Presence) {
    engine
        .remote_presence(fixture.member.peer, generation, value)
        .unwrap();
}

#[tokio::test]
async fn identical_normalized_presence_needs_no_writer_and_still_notifies_and_tracks_generation() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let value = advertised();
    publish(&mut engine, &fixture, 7, &value);
    let observer = fixture.observer();
    let before = version(&observer);
    observer.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Exercise the cache update even when the persisted online row is identical.
    engine.remote_generations.remove(&fixture.member.peer);
    let notification = engine.arrival_notification();
    let notified = notification.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let mut same = value.clone();
    same.sessions[0].person = "another assertion".into();
    same.sessions[0].machine = "another assertion".into();
    same.sessions[0].address = "another assertion".into();
    same.sessions[0].is_me = false;
    same.known = vec!["river/desk/saved".into()];
    same.kept = vec!["river/desk/retained".into()];
    publish(&mut engine, &fixture, 7, &same);
    assert_eq!(engine.remote_generations[&fixture.member.peer], (7, true));
    tokio::time::timeout(Duration::from_secs(1), notified)
        .await
        .unwrap();
    observer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(version(&observer), before);
}

#[test]
fn identical_presence_does_not_commit_but_each_complete_payload_change_does() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let mut value = advertised();
    publish(&mut engine, &fixture, 8, &value);
    let observer = fixture.observer();
    let mut previous = version(&observer);
    publish(&mut engine, &fixture, 8, &value);
    assert_eq!(version(&observer), previous);
    for field in 0..4 {
        match field {
            0 => value.sessions[0].status = "occupied".into(),
            1 => value.known.push("second".into()),
            2 => value.kept.push("third".into()),
            _ => value.features.push("new capability".into()),
        }
        publish(&mut engine, &fixture, 8, &value);
        let current = version(&observer);
        assert_ne!(current, previous);
        previous = current;
    }
    let snapshot = engine.mail.remote_snapshot().unwrap().unwrap();
    assert_eq!(snapshot.presence[0].presence.sessions[0].status, "occupied");
    assert_eq!(snapshot.presence[0].presence.features, value.features);
    drop(engine);
    let reopened = fixture.engine();
    let snapshot = reopened.mail.remote_snapshot().unwrap().unwrap();
    assert_eq!(snapshot.presence[0].presence.features, value.features);
    assert_eq!(snapshot.presence[0].presence.known.len(), 2);
    assert_eq!(snapshot.presence[0].presence.kept.len(), 2);
}

#[test]
fn generation_offline_reconnect_and_team_changes_are_not_noops() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let value = advertised();
    publish(&mut engine, &fixture, 10, &value);
    let observer = fixture.observer();
    let before = version(&observer);
    publish(&mut engine, &fixture, 11, &value);
    assert_ne!(version(&observer), before);
    engine.remote_disconnected(fixture.member.peer, 11).unwrap();
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 11, &value),
        Err(EngineError::StaleAttempt)
    ));
    let before = version(&observer);
    publish(&mut engine, &fixture, 12, &value);
    assert_ne!(version(&observer), before);
    let snapshot = engine.mail.remote_snapshot().unwrap().unwrap();
    assert!(snapshot.presence[0].online);
    assert_eq!(snapshot.presence[0].generation, 12);
    engine
        .sync_remote_members("another", 1, std::slice::from_ref(&fixture.member))
        .unwrap();
    let before = version(&observer);
    publish(&mut engine, &fixture, 12, &value);
    assert_ne!(version(&observer), before);
    drop(engine);
    let reopened = fixture.engine();
    let snapshot = reopened.mail.remote_snapshot().unwrap().unwrap();
    assert_eq!(snapshot.team, "another");
    assert_eq!(snapshot.presence[0].generation, 12);
    assert!(!snapshot.presence[0].online);
}

#[test]
fn identical_presence_still_rejects_stale_revoked_suspended_and_invalid_input() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let value = advertised();
    publish(&mut engine, &fixture, 3, &value);
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 2, &value),
        Err(EngineError::StaleAttempt)
    ));
    let mut invalid = value.clone();
    invalid.sessions.push(invalid.sessions[0].clone());
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 3, &invalid),
        Err(EngineError::Conflict)
    ));
    invalid = value.clone();
    invalid.sessions[0].status.push('\n');
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 3, &invalid),
        Err(EngineError::InvalidInput)
    ));
    invalid = value.clone();
    invalid.features = vec!["x".repeat(crate::codec::MAX_FRAME_BYTES)];
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 3, &invalid),
        Err(EngineError::Limit)
    ));
    engine.suspend_remote_directory();
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 3, &value),
        Err(EngineError::Unauthorized)
    ));
    engine.sync_remote_members("current", 2, &[]).unwrap();
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 3, &value),
        Err(EngineError::Unauthorized)
    ));
}

#[test]
fn identical_input_still_detects_corrupted_durable_presence_and_latches_uncertainty() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let value = advertised();
    publish(&mut engine, &fixture, 1, &value);
    fixture
        .observer()
        .execute("UPDATE remote_presence SET payload=x'ff'", [])
        .unwrap();
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 1, &value),
        Err(EngineError::StorageUncertain)
    ));
    assert!(engine.uncertain);
}

#[test]
fn identical_input_still_checks_database_inode_protection() {
    let fixture = Fixture::new();
    let mut engine = fixture.ready();
    let value = advertised();
    publish(&mut engine, &fixture, 1, &value);
    let database = fixture.root.join("mail.sqlite3");
    let displaced = fixture.root.join("original.sqlite3");
    fs::rename(&database, &displaced).unwrap();
    fs::write(&database, b"replacement").unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        engine.remote_presence(fixture.member.peer, 1, &value),
        Err(EngineError::StorageUncertain)
    ));
    assert_eq!(fs::read(&database).unwrap(), b"replacement");
    assert!(engine.uncertain);
}
