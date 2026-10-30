#![cfg(unix)]
use foundry_network::daemon_config::{ConfigError, DaemonConfig};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("config-{}", iroh::SecretKey::generate().public()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn value(&self) -> Value {
        json!({"state_directory":self.0,"socket_directory":self.0,"person":"alice","machine":"laptop","network":"disabled","folders":[]})
    }
    fn write(&self, value: &Value) -> PathBuf {
        let path = self.0.join("config.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn explicit_offline_config_loads_without_creating_state_or_profiles() {
    let root = Root::new();
    let path = root.write(&root.value());
    let config = DaemonConfig::load(&path).unwrap();
    assert_eq!(config.person, "alice");
    assert_eq!(config.state_directory, root.0);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
}

#[test]
fn unknown_missing_malformed_and_unbounded_values_fail() {
    let root = Root::new();
    let mut cases = Vec::new();
    let mut unknown = root.value();
    unknown["token"] = json!("secret-marker");
    cases.push(unknown);
    let mut missing = root.value();
    missing.as_object_mut().unwrap().remove("network");
    cases.push(missing);
    for (field, value) in [
        ("person", json!("")),
        ("machine", json!("x".repeat(513))),
        ("state_directory", json!("relative")),
        ("network", json!("hosted")),
        (
            "folders",
            json!([{"path":root.0,"allow":true,"unknown":true}]),
        ),
    ] {
        let mut invalid = root.value();
        invalid[field] = value;
        cases.push(invalid);
    }
    for value in cases {
        let path = root.write(&value);
        assert!(matches!(
            DaemonConfig::load(&path),
            Err(ConfigError::Invalid)
        ));
    }
    let path = root.0.join("config.json");
    fs::write(&path, b"{").unwrap();
    assert!(matches!(
        DaemonConfig::load(&path),
        Err(ConfigError::Invalid)
    ));
    fs::write(&path, vec![b' '; 65537]).unwrap();
    assert!(matches!(
        DaemonConfig::load(&path),
        Err(ConfigError::Invalid)
    ));
    assert!(!ConfigError::Invalid.to_string().contains("secret-marker"));
}

#[test]
fn writable_linked_and_nonregular_files_are_rejected() {
    let root = Root::new();
    let path = root.write(&root.value());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        DaemonConfig::load(&path),
        Err(ConfigError::UnsafeFile)
    ));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let linked = root.0.join("linked");
    fs::hard_link(&path, &linked).unwrap();
    assert!(matches!(
        DaemonConfig::load(&path),
        Err(ConfigError::UnsafeFile)
    ));
    fs::remove_file(linked).unwrap();
    let alias = root.0.join("alias");
    symlink(&path, &alias).unwrap();
    assert!(DaemonConfig::load(&alias).is_err());
    assert!(matches!(
        DaemonConfig::load(&root.0),
        Err(ConfigError::UnsafeFile)
    ));
}
