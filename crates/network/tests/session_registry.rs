// Compile the production storage protection internally without shared exports.
mod persistence {
    pub use foundry_network::persistence::{MEMBERSHIP_DATABASE, PersistenceError, Result};
    use rusqlite::{Connection, TransactionBehavior};
    use std::{path::Path, time::Duration};
    #[allow(dead_code)]
    pub mod storage {
        include!("../src/persistence_storage.rs");
    }
}
#[path = "../src/session_registry.rs"]
mod session_registry;
use session_registry::*;
use std::path::PathBuf;

fn registration(name: &str, share_path: bool) -> Registration {
    Registration {
        conversation: Conversation {
            person: "alice".into(),
            machine: "desktop".into(),
            name: name.into(),
        },
        repository: std::env::current_dir().unwrap(),
        share_path,
    }
}
fn registry() -> SessionRegistry {
    SessionRegistry::new(FolderPolicy::default())
}
fn named(name: &str) -> DirectoryFilter {
    DirectoryFilter {
        name: Some(name.into()),
        ..Default::default()
    }
}
#[test]
fn credentials_cannot_cross_sessions_even_on_a_multiplexed_connection() {
    let mut registry = registry();
    let a = registry.register(registration("editor", false)).unwrap();
    let b = registry.register(registration("review", false)).unwrap();
    let connection = ConnectionId(1);
    registry
        .attach(
            &a.conversation,
            &a.credential,
            connection,
            DeliveryCapability::Pull,
        )
        .unwrap();
    registry
        .attach(
            &b.conversation,
            &b.credential,
            connection,
            DeliveryCapability::Hook,
        )
        .unwrap();
    assert_eq!(registry.linked(connection), Err(RegistryError::Ambiguous));
    assert_eq!(
        registry.authorize_connection(&b.conversation, &a.credential, connection),
        Err(RegistryError::Unauthorized)
    );
    assert_eq!(
        registry.set_status(
            &b.conversation,
            &a.credential,
            connection,
            "busy",
            "reviewing"
        ),
        Err(RegistryError::Unauthorized)
    );
    let wrong = SessionCredential::from_bytes([0; 32]);
    assert_eq!(
        registry.attach(
            &a.conversation,
            &wrong,
            ConnectionId(9),
            DeliveryCapability::Push
        ),
        Err(RegistryError::Unauthorized)
    );
    assert_eq!(a.credential.as_bytes().len(), 32);
    assert!(!format!("{:?}", a.credential).contains(&format!("{:?}", a.credential.as_bytes())));
}
#[test]
fn reconnect_keeps_identity_status_and_offline_address() {
    let mut registry = registry();
    let a = registry.register(registration("editor", false)).unwrap();
    registry
        .attach(
            &a.conversation,
            &a.credential,
            ConnectionId(1),
            DeliveryCapability::Hook,
        )
        .unwrap();
    registry
        .attach(
            &a.conversation,
            &a.credential,
            ConnectionId(2),
            DeliveryCapability::Pull,
        )
        .unwrap();
    registry
        .set_status(
            &a.conversation,
            &a.credential,
            ConnectionId(1),
            "busy",
            "compiling",
        )
        .unwrap();
    registry.disconnect(ConnectionId(1));
    assert_eq!(
        registry
            .resolve(&named("editor"))
            .unwrap()
            .associations
            .len(),
        1
    );
    registry.disconnect(ConnectionId(2));
    let entry = registry.who(&named("editor"), 1).unwrap().remove(0);
    assert!(!entry.online);
    assert!(entry.capabilities.is_empty());
    assert_eq!(
        (entry.status.as_str(), entry.doing.as_str()),
        ("busy", "compiling")
    );
    assert!(
        registry
            .resolve(&named("editor"))
            .unwrap()
            .associations
            .is_empty()
    );
    assert_eq!(
        registry
            .register(registration("editor", false))
            .unwrap_err(),
        RegistryError::Unauthorized
    );
    assert_eq!(
        registry.authorize_connection(&a.conversation, &a.credential, ConnectionId(2)),
        Err(RegistryError::Unauthorized)
    );
    registry
        .attach(
            &a.conversation,
            &a.credential,
            ConnectionId(3),
            DeliveryCapability::Push,
        )
        .unwrap();
    assert_eq!(
        registry.linked(ConnectionId(3)).unwrap().conversation,
        a.conversation
    );
    assert_eq!(
        registry.who(&named("editor"), 1).unwrap()[0].capabilities,
        vec![DeliveryCapability::Push]
    );
}
#[test]
fn directory_rejects_ambiguity_and_keeps_private_paths_unsearchable() {
    let mut registry = registry();
    registry.register(registration("editor", false)).unwrap();
    let mut other = registration("editor", true);
    other.conversation.person = "bob".into();
    other.conversation.machine = "laptop".into();
    registry.register(other).unwrap();
    assert_eq!(
        registry.resolve(&named("editor")),
        Err(RegistryError::Ambiguous)
    );
    let filter = DirectoryFilter {
        person: Some("alice".into()),
        machine: Some("desktop".into()),
        ..Default::default()
    };
    assert!(registry.who(&filter, 1).unwrap()[0].repository.is_none());
    let filter = DirectoryFilter {
        repository: Some(std::env::current_dir().unwrap()),
        ..Default::default()
    };
    assert_eq!(
        registry.who(&filter, 1).unwrap()[0].conversation.person,
        "bob"
    );
    let filter = DirectoryFilter {
        online_only: true,
        ..Default::default()
    };
    assert!(registry.who(&filter, 1).unwrap().is_empty());
}
#[test]
fn bounds_fail_explicitly_without_truncation_or_partial_status_update() {
    let mut registry = registry();
    let a = registry.register(registration("first", false)).unwrap();
    for id in 0..MAX_ASSOCIATIONS {
        registry
            .attach(
                &a.conversation,
                &a.credential,
                ConnectionId(id as u64),
                DeliveryCapability::Pull,
            )
            .unwrap();
    }
    assert_eq!(
        registry.attach(
            &a.conversation,
            &a.credential,
            ConnectionId(999),
            DeliveryCapability::Pull
        ),
        Err(RegistryError::Limit)
    );
    let oversized = "x".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(
        registry.set_status(
            &a.conversation,
            &a.credential,
            ConnectionId(0),
            "busy",
            &oversized
        ),
        Err(RegistryError::InvalidText)
    );
    assert_eq!(registry.who(&named("first"), 1).unwrap()[0].status, "");
    assert_eq!(
        registry.who(&named(&oversized), 1),
        Err(RegistryError::InvalidText)
    );
    assert_eq!(
        registry.who(&DirectoryFilter::default(), MAX_RESULTS + 1),
        Err(RegistryError::Limit)
    );
    assert_eq!(
        registry.who(&DirectoryFilter::default(), 0),
        Err(RegistryError::Limit)
    );
    for id in 1..MAX_SESSIONS {
        registry
            .register(registration(&format!("session-{id}"), false))
            .unwrap();
    }
    assert_eq!(
        registry
            .register(registration("overflow", false))
            .unwrap_err(),
        RegistryError::Limit
    );
    assert_eq!(
        registry.who(&DirectoryFilter::default(), MAX_RESULTS),
        Err(RegistryError::Limit)
    );
}
#[test]
fn folder_policy_blocks_existing_visibility_delivery_and_registration() {
    let root = std::env::current_dir().unwrap();
    let mut registry = registry();
    let a = registry.register(registration("editor", true)).unwrap();
    registry
        .attach(
            &a.conversation,
            &a.credential,
            ConnectionId(1),
            DeliveryCapability::Hook,
        )
        .unwrap();
    registry.set_policy(FolderPolicy::new(vec![(root.clone(), false)]).unwrap());
    assert!(
        registry
            .who(&DirectoryFilter::default(), 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        registry.resolve(&named("editor")),
        Err(RegistryError::NotFound)
    );
    assert_eq!(
        registry.authorize_connection(&a.conversation, &a.credential, ConnectionId(1)),
        Err(RegistryError::Excluded)
    );
    assert_eq!(
        registry.attach(
            &a.conversation,
            &a.credential,
            ConnectionId(2),
            DeliveryCapability::Hook
        ),
        Err(RegistryError::Excluded)
    );
    assert_eq!(
        registry.register(registration("other", true)).unwrap_err(),
        RegistryError::Excluded
    );
    registry.set_policy(
        FolderPolicy::new(vec![(root.parent().unwrap().into(), false), (root, true)]).unwrap(),
    );
    assert!(registry.resolve(&named("editor")).is_ok());
    assert!(FolderPolicy::new(vec![(PathBuf::from("/missing-folder-for-test"), false)]).is_err());
}
#[cfg(unix)]
#[test]
fn excluded_folder_cannot_be_registered_through_symlink() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("foundry-sessions-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let hidden = root.join("hidden");
    std::fs::create_dir(&hidden).unwrap();
    let alias = root.join("alias");
    symlink(&hidden, &alias).unwrap();
    let mut registry = SessionRegistry::new(FolderPolicy::new(vec![(hidden, false)]).unwrap());
    let mut input = registration("editor", false);
    input.repository = alias;
    assert_eq!(
        registry.register(input).unwrap_err(),
        RegistryError::Excluded
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn durable_api_requires_an_absolute_storage_root() {
    assert_eq!(SESSION_DATABASE, "sessions.sqlite3");
    assert!(matches!(
        DurableRegistry::open(
            std::path::Path::new("relative"),
            FolderPolicy::default(),
            std::time::Duration::from_millis(1)
        ),
        Err(DurableError::Storage)
    ));
}
