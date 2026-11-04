//! Folder policy for session paths reached through symbolic links.
#![cfg(unix)]
use foundry_network::session_registry::*;
use std::{fs, os::unix::fs::symlink, path::PathBuf, time::Duration};

/// A fresh folder with `off` and `open` children, removed on drop. The temp root
/// is left unresolved so platforms whose temp root is itself a link are covered.
struct Folders {
    root: PathBuf,
    off: PathBuf,
    open: PathBuf,
}
impl Folders {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "foundry-folder-links-{:x}",
            u64::from_ne_bytes(random)
        ));
        fs::create_dir(&root).unwrap();
        let (off, open) = (root.join("off"), root.join("open"));
        fs::create_dir(&off).unwrap();
        fs::create_dir(&open).unwrap();
        Self { root, off, open }
    }
}
impl Drop for Folders {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn at(name: &str, repository: PathBuf) -> Registration {
    Registration {
        conversation: Conversation {
            person: "alice".into(),
            machine: "desktop".into(),
            name: name.into(),
        },
        repository,
        share_path: false,
    }
}
fn named(name: &str) -> DirectoryFilter {
    DirectoryFilter {
        name: Some(name.into()),
        ..Default::default()
    }
}
fn assert_excluded(registry: &mut SessionRegistry, path: PathBuf) {
    assert_eq!(
        registry.register(at("excluded", path)).unwrap_err(),
        RegistryError::Excluded
    );
}
/// A session admitted before the rule existed is hidden and blocked once it does.
fn assert_hidden_after_policy(link: PathBuf, policy: FolderPolicy) {
    let mut registry = SessionRegistry::new(FolderPolicy::default());
    let a = registry.register(at("editor", link)).unwrap();
    registry.set_policy(policy);
    assert_eq!(
        registry.resolve(&named("editor")),
        Err(RegistryError::NotFound)
    );
    assert_eq!(
        registry.attach(
            &a.conversation,
            &a.credential,
            ConnectionId(1),
            DeliveryCapability::Hook
        ),
        Err(RegistryError::Excluded)
    );
}

#[test]
fn link_inside_excluded_folder_cannot_register_its_allowed_target() {
    let folders = Folders::new();
    let link = folders.off.join("link");
    symlink(&folders.open, &link).unwrap();
    let policy = || FolderPolicy::new(vec![(folders.off.clone(), false)]).unwrap();
    let mut registry = SessionRegistry::new(policy());
    assert_excluded(&mut registry, link.clone());
    assert_excluded(&mut registry, folders.open.join("../off/link"));
    registry
        .register(at("target", folders.open.clone()))
        .unwrap();
    assert!(registry.resolve(&named("target")).is_ok());
    assert_hidden_after_policy(link, policy());
}
#[test]
fn link_inside_allowed_folder_cannot_register_its_excluded_target() {
    let folders = Folders::new();
    let link = folders.open.join("link");
    symlink(&folders.off, &link).unwrap();
    let rules = vec![(folders.off.clone(), false), (folders.open.clone(), true)];
    let mut registry = SessionRegistry::new(FolderPolicy::new(rules.clone()).unwrap());
    assert_excluded(&mut registry, link.clone());
    assert_hidden_after_policy(link, FolderPolicy::new(rules).unwrap());
}
#[test]
fn plain_folders_keep_the_deepest_rule_and_missing_folders_stay_invalid() {
    let folders = Folders::new();
    let kept = folders.off.join("kept");
    fs::create_dir(&kept).unwrap();
    let rules = vec![(folders.off.clone(), false), (kept.clone(), true)];
    let mut registry = SessionRegistry::new(FolderPolicy::new(rules).unwrap());
    registry.register(at("open", folders.open.clone())).unwrap();
    registry.register(at("kept", kept)).unwrap();
    registry
        .register(at("dotted", folders.off.join("..").join("open")))
        .unwrap();
    assert_excluded(&mut registry, folders.off.clone());
    assert_excluded(&mut registry, folders.open.join("..").join("off"));
    for missing in [folders.open.join("missing"), folders.off.join("missing")] {
        assert_eq!(
            registry.register(at("missing", missing)).unwrap_err(),
            RegistryError::InvalidPath
        );
    }
    assert_eq!(
        registry.who(&DirectoryFilter::default(), 8).unwrap().len(),
        3
    );
}
#[test]
fn restart_keeps_judging_the_path_as_given() {
    let folders = Folders::new();
    let link = folders.off.join("link");
    symlink(&folders.open, &link).unwrap();
    let state = folders.root.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let open = |policy| DurableRegistry::open(&state, policy, Duration::from_millis(10)).unwrap();
    let mut store = open(FolderPolicy::default());
    store.transact(|r| r.register(at("editor", link))).unwrap();
    drop(store);
    let store = open(FolderPolicy::new(vec![(folders.off.clone(), false)]).unwrap());
    let registry = store.registry().unwrap();
    assert_eq!(
        registry.resolve(&named("editor")),
        Err(RegistryError::NotFound)
    );
    drop(store);
    let store = open(FolderPolicy::default());
    assert!(store.registry().unwrap().resolve(&named("editor")).is_ok());
}
