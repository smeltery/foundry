use foundry_network::daemon_config::{DaemonConfig, NetworkConfig, NetworkMode};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

pub struct Root(pub PathBuf);
impl Root {
    pub fn new() -> Self {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).unwrap();
        let path = PathBuf::from("/tmp").join(format!(
            "network-config-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub fn write(&self, path: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(path);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
    pub fn config(&self, network: Value) -> Value {
        json!({"state_directory":self.0.join("state-not-created"),
            "socket_directory":self.0,"person":"alex","machine":"desk","folders":[],"network":network})
    }
    pub fn load(&self, network: Value) -> DaemonConfig {
        let path = self.write(
            "daemon.json",
            &serde_json::to_vec(&self.config(network)).unwrap(),
        );
        DaemonConfig::load(&path).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
pub fn network() -> Value {
    json!({"mode":"enabled","profile":profile(),
        "transport":{"bind_address":"127.0.0.1:0","direct_paths":true,
            "relays":[],"relay_root_certificates":[]},
        "seeds":[],"trust":{"mode":"unverified"},"codebook":null,
        "ticket_relay_compression":null,"expected_team":null,"startup":{"mode":"existing"}})
}
pub fn profile() -> Value {
    json!({"team_alpn":[102,120,116],"join_alpn":[102,120,106],"protocol":73,
        "signing_prefix":"config-fixture-signatures/","invitation_endpoint_context":"config-fixture-endpoints",
        "invitation_secret_context":"config-fixture-secrets","ticket_prefixes":["fixture73"]})
}
pub fn enabled(config: &DaemonConfig) -> &NetworkConfig {
    match &config.network {
        NetworkMode::Enabled(value) => value,
        NetworkMode::Disabled => panic!("expected explicit network"),
    }
}
pub fn codebook(root: &Root) -> Value {
    let bytes = b"birch\ncobalt\ndahlia\nelm\n";
    root.write("dictionary.txt", bytes);
    json!({"path":"dictionary.txt","sha256":data_encoding::HEXLOWER.encode(
        ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())})
}
