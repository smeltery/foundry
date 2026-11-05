//! How configured folder rule entries are read: defaults, `..` and linked roots.
#![cfg(unix)]
use foundry_network::{daemon_config::DaemonConfig, session_registry::*};
use serde_json::json;
use std::{fs, os::unix::fs::symlink, path::PathBuf};

/// A fresh `base` with `off` and `open` children plus an `alias` link to `base`.
struct Tree {
    base: PathBuf,
    alias: PathBuf,
}
impl Tree {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let tag = u64::from_ne_bytes(random);
        let base = std::env::temp_dir().join(format!("foundry-rule-entries-{tag:x}"));
        for child in ["off", "open"] {
            fs::create_dir_all(base.join(child)).unwrap();
        }
        let alias = std::env::temp_dir().join(format!("foundry-rule-alias-{tag:x}"));
        symlink(&base, &alias).unwrap();
        Self { base, alias }
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_file(&self.alias).unwrap();
        fs::remove_dir_all(&self.base).unwrap();
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
fn outcome(policy: FolderPolicy, path: PathBuf) -> std::result::Result<(), RegistryError> {
    SessionRegistry::new(policy)
        .register(at("probe", path))
        .map(|_| ())
}
fn config(folders: serde_json::Value) -> serde_json::Result<DaemonConfig> {
    let state = std::env::temp_dir();
    serde_json::from_value(json!({"state_directory":state,"socket_directory":state,
        "person":"alice","machine":"laptop","network":"disabled","folders":folders}))
}

#[test]
fn no_rules_exclude_nothing_and_rule_entries_fail_closed() {
    let tree = Tree::new();
    let empty = config(json!([])).unwrap().folder_policy().unwrap();
    let mut registry = SessionRegistry::new(empty);
    for (name, child) in [("off", "off"), ("open", "open"), ("base", "")] {
        registry.register(at(name, tree.base.join(child))).unwrap();
    }
    // A rule value is a boolean decision; any other spelling is a configuration error.
    let off = tree.base.join("off");
    for rule in [
        json!({"path":off,"allow":"off"}),
        json!({"path":off,"allow":null}),
        json!({"path":off}),
        json!({"path":off,"allow":false,"mode":"off"}),
    ] {
        assert!(config(json!([rule])).is_err(), "{rule}");
    }
    let missing = config(json!([{"path":tree.base.join("missing"),"allow":false}])).unwrap();
    assert!(missing.folder_policy().is_err());
    let duplicate = json!([{"path":off,"allow":false},{"path":off,"allow":true}]);
    assert!(config(duplicate).unwrap().folder_policy().is_err());
}

#[test]
fn dotted_rule_entries_name_the_folder_they_resolve_to() {
    let tree = Tree::new();
    let dotted = tree.base.join("open").join("..").join("off");
    let policy = || FolderPolicy::new(vec![(dotted.clone(), false)]).unwrap();
    for path in [
        tree.base.join("off"),
        dotted.clone(),
        tree.base.join("open/../off/."),
    ] {
        assert_eq!(outcome(policy(), path), Err(RegistryError::Excluded));
    }
    assert_eq!(outcome(policy(), tree.base.join("open")), Ok(()));
    // The folder must exist when the policy is built; a future folder is not a rule.
    let future = tree.base.join("open").join("..").join("later");
    assert_eq!(
        FolderPolicy::new(vec![(future, false)]).unwrap_err(),
        RegistryError::InvalidPath
    );
}

#[test]
fn rules_written_through_a_linked_ancestor_cover_both_spellings() {
    let tree = Tree::new();
    let inner = tree.base.join("off").join("into-open");
    symlink(tree.base.join("open"), &inner).unwrap();
    for rule_root in [&tree.alias, &tree.base] {
        let policy = || FolderPolicy::new(vec![(rule_root.join("off"), false)]).unwrap();
        for session_root in [&tree.alias, &tree.base] {
            for child in ["off", "off/into-open"] {
                assert_eq!(
                    outcome(policy(), session_root.join(child)),
                    Err(RegistryError::Excluded),
                    "rule under {rule_root:?}, session {child} under {session_root:?}"
                );
            }
            assert_eq!(outcome(policy(), session_root.join("open")), Ok(()));
        }
    }
}
