#![cfg(unix)]
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let root =
            std::env::temp_dir().join(format!("foundry-registry-{:x}", u64::from_ne_bytes(random)));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn open(&self) -> DurableRegistry {
        DurableRegistry::open(&self.0, FolderPolicy::default(), Duration::from_millis(10)).unwrap()
    }
    fn sql(&self) -> Connection {
        Connection::open(self.0.join(SESSION_DATABASE)).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn input(name: &str) -> Registration {
    Registration {
        conversation: Conversation {
            person: "person".into(),
            machine: "machine".into(),
            name: name.into(),
        },
        repository: std::env::current_dir().unwrap(),
        share_path: false,
    }
}
fn all() -> DirectoryFilter {
    DirectoryFilter::default()
}
#[test]
fn restart_preserves_secrets_status_and_offline_identity() {
    let root = Root::new();
    let mut store = root.open();
    let a = store.transact(|r| r.register(input("editor"))).unwrap();
    let b = store.transact(|r| r.register(input("review"))).unwrap();
    store
        .transact(|r| {
            r.attach(
                &a.conversation,
                &a.credential,
                ConnectionId(1),
                DeliveryCapability::Hook,
            )?;
            r.set_status(
                &a.conversation,
                &a.credential,
                ConnectionId(1),
                "busy",
                "testing",
            )
        })
        .unwrap();
    drop(store);
    let mut store = root.open();
    let entries = store.registry().unwrap().who(&all(), 2).unwrap();
    assert!(
        entries
            .iter()
            .all(|e| !e.online && e.capabilities.is_empty() && e.repository.is_none())
    );
    assert_eq!(entries[0].status, "busy");
    assert_eq!(entries[0].doing, "testing");
    assert!(
        store
            .registry()
            .unwrap()
            .resolve(&DirectoryFilter {
                name: Some("editor".into()),
                ..all()
            })
            .unwrap()
            .associations
            .is_empty()
    );
    assert_eq!(
        store.transact(|r| r.attach(
            &a.conversation,
            &b.credential,
            ConnectionId(2),
            DeliveryCapability::Pull
        )),
        Err(DurableError::Registry(RegistryError::Unauthorized))
    );
    store
        .transact(|r| {
            r.attach(
                &a.conversation,
                &a.credential,
                ConnectionId(2),
                DeliveryCapability::Pull,
            )
        })
        .unwrap();
    assert_eq!(
        store
            .registry()
            .unwrap()
            .linked(ConnectionId(2))
            .unwrap()
            .conversation,
        a.conversation
    );
}
#[test]
fn failed_transaction_never_publishes_staged_authority_and_requires_reopen() {
    let root = Root::new();
    let mut store = root.open();
    let lock = root.sql();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        store
            .transact(|r| r.register(input("blocked")))
            .unwrap_err(),
        DurableError::Busy
    );
    assert!(matches!(
        store.registry(),
        Err(DurableError::ReloadRequired)
    ));
    lock.execute_batch("ROLLBACK").unwrap();
    drop(store);
    let store = root.open();
    assert!(store.registry().unwrap().who(&all(), 1).unwrap().is_empty());
}
#[test]
fn stale_writer_cannot_replace_committed_identity() {
    let root = Root::new();
    let mut first = root.open();
    let mut stale = root.open();
    first.transact(|r| r.register(input("kept"))).unwrap();
    assert_eq!(
        stale.transact(|r| r.register(input("lost"))).unwrap_err(),
        DurableError::Conflict
    );
    assert!(matches!(
        stale.registry(),
        Err(DurableError::ReloadRequired)
    ));
    drop((first, stale));
    assert_eq!(
        root.open().registry().unwrap().who(&all(), 1).unwrap()[0]
            .conversation
            .name,
        "kept"
    );
}
#[test]
fn folder_policy_is_revalidated_after_restart_without_losing_identity() {
    let root = Root::new();
    let mut store = root.open();
    let a = store.transact(|r| r.register(input("editor"))).unwrap();
    drop(store);
    let policy = FolderPolicy::new(vec![(std::env::current_dir().unwrap(), false)]).unwrap();
    let mut store = DurableRegistry::open(&root.0, policy, Duration::from_millis(10)).unwrap();
    assert!(store.registry().unwrap().who(&all(), 1).unwrap().is_empty());
    assert_eq!(
        store.transact(|r| r.attach(
            &a.conversation,
            &a.credential,
            ConnectionId(1),
            DeliveryCapability::Hook
        )),
        Err(DurableError::Registry(RegistryError::Excluded))
    );
    drop(store);
    assert_eq!(
        root.open().registry().unwrap().who(&all(), 1).unwrap()[0].conversation,
        a.conversation
    );
}
#[test]
fn malformed_schema_payload_and_excess_bounds_fail_closed() {
    let root = Root::new();
    drop(root.open());
    let sql = root.sql();
    for payload in [
        b"{}".as_slice(),
        b"{\"sessions\":[],\"extra\":1}",
        b"not-json",
    ] {
        sql.execute("UPDATE registry SET payload=?1", [payload])
            .unwrap();
        assert!(matches!(
            DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
            Err(DurableError::Corrupt)
        ));
    }
    sql.execute(
        "UPDATE registry SET payload=zeroblob(?1)",
        [document::MAX_BYTES as i64 + 1],
    )
    .unwrap();
    assert!(matches!(
        DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
        Err(DurableError::Corrupt)
    ));
    sql.execute_batch("CREATE TRIGGER invalid AFTER UPDATE ON registry BEGIN SELECT 1; END;")
        .unwrap();
    assert!(matches!(
        DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
        Err(DurableError::Schema)
    ));
}
#[test]
fn protected_storage_rejects_public_permissions_and_symlinks() {
    let root = Root::new();
    drop(root.open());
    let database = root.0.join(SESSION_DATABASE);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
        Err(DurableError::UnsafeStorage)
    ));
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    let moved = root.0.join("moved");
    fs::rename(&database, &moved).unwrap();
    symlink(moved, database).unwrap();
    assert!(matches!(
        DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
        Err(DurableError::UnsafeStorage)
    ));
}

#[test]
fn malformed_records_cannot_expand_authority_or_bypass_bounds() {
    let root = Root::new();
    let mut store = root.open();
    store.transact(|r| r.register(input("editor"))).unwrap();
    drop(store);
    let sql = root.sql();
    let bytes: Vec<u8> = sql
        .query_row("SELECT payload FROM registry", [], |r| r.get(0))
        .unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut duplicate = original.clone();
    duplicate["sessions"]
        .as_array_mut()
        .unwrap()
        .push(original["sessions"][0].clone());
    let mut oversized = original.clone();
    oversized["sessions"][0]["status"] = "x".repeat(MAX_TEXT_BYTES + 1).into();
    let mut excessive = original.clone();
    excessive["sessions"] =
        serde_json::Value::Array(vec![original["sessions"][0].clone(); MAX_SESSIONS + 1]);
    let mut secret = original.clone();
    secret["sessions"][0]["credential"] = serde_json::json!([1]);
    for invalid in [duplicate, oversized, excessive, secret] {
        sql.execute(
            "UPDATE registry SET payload=?1",
            [serde_json::to_vec(&invalid).unwrap()],
        )
        .unwrap();
        assert!(matches!(
            DurableRegistry::open(&root.0, FolderPolicy::default(), Duration::from_millis(10)),
            Err(DurableError::Corrupt)
        ));
    }
}

#[test]
fn database_full_rolls_back_status_and_registration() {
    let root = Root::new();
    let mut store = root.open();
    let a = store
        .transact(|r| {
            let a = r.register(input("editor"))?;
            r.attach(
                &a.conversation,
                &a.credential,
                ConnectionId(1),
                DeliveryCapability::Pull,
            )?;
            r.set_status(&a.conversation, &a.credential, ConnectionId(1), "ready", "")?;
            Ok(a)
        })
        .unwrap();
    store
        .connection
        .execute_batch("PRAGMA max_page_count=2")
        .unwrap();
    assert_eq!(
        store.transact(|r| {
            r.set_status(&a.conversation, &a.credential, ConnectionId(1), "busy", "")?;
            for index in 0..100 {
                r.register(input(&format!("extra-{index}")))?;
            }
            Ok(())
        }),
        Err(DurableError::Storage)
    );
    assert!(matches!(
        store.registry(),
        Err(DurableError::ReloadRequired)
    ));
    drop(store);
    let store = root.open();
    let entries = store.registry().unwrap().who(&all(), 1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, "ready");
    assert!(!entries[0].online);
}
