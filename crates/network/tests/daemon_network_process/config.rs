use super::clock::Clock;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const WORDS: &[u8] = b"birch\ncobalt\ndahlia\nelm\n";
pub struct Root(pub PathBuf);
impl Root {
    pub fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            PathBuf::from("/tmp").join(format!("f5yn-{}", data_encoding::HEXLOWER.encode(&nonce)));
        private_dir(&path);
        Self(path.canonicalize().unwrap())
    }
    pub fn config(&self, label: &str, person: &str) -> Config {
        let home = self.0.join(label);
        private_dir(&home);
        for name in ["state", "socket", "settings", "workspace", "cwd"] {
            private_dir(&home.join(name));
        }
        let state = home.join("state");
        let socket = home.join("socket");
        let repository = home.join("workspace");
        let path = home.join("settings/daemon.json");
        let value = json!({"state_directory":state,"socket_directory":socket,"person":person,
            "machine":format!("desk-{label}"),"folders":[{"path":repository,"allow":true}],
            "network":{"mode":"enabled","profile":profile(),
                "transport":{"bind_address":"127.0.0.1:0","direct_paths":true,"relays":[],"relay_root_certificates":[]},
                "seeds":[],"trust":{"mode":"unverified"},
                "codebook":{"path":"words.txt","sha256":data_encoding::HEXLOWER.encode(ring::digest::digest(&ring::digest::SHA256, WORDS).as_ref())},
                "startup":{"mode":"existing"}}});
        let config = Config {
            path,
            state,
            socket,
            repository,
            cwd: home.join("cwd"),
            person: person.into(),
            name: format!("terminal-{label}"),
            value,
            clock: None,
        };
        config.reference("words.txt", WORDS);
        config.save();
        config
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Clone)]
pub struct Config {
    pub path: PathBuf,
    pub state: PathBuf,
    pub socket: PathBuf,
    pub repository: PathBuf,
    pub cwd: PathBuf,
    pub person: String,
    pub name: String,
    pub value: Value,
    pub clock: Option<Clock>,
}
impl Config {
    pub fn save(&self) {
        private_file(&self.path, &serde_json::to_vec(&self.value).unwrap());
    }
    pub fn reference(&self, name: &str, bytes: &[u8]) {
        private_file(&self.path.parent().unwrap().join(name), bytes);
    }
    pub fn destination(&self) -> Value {
        json!({"person":self.person,"name":self.name})
    }
    pub fn relay_only(&mut self, url: &str, roots: &[Vec<u8>]) {
        let paths: Vec<_> = roots
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                let name = format!("relay-{i}.der");
                self.reference(&name, bytes);
                name
            })
            .collect();
        self.value["network"]["transport"] = json!({"bind_address":"127.0.0.1:0","direct_paths":false,
            "relays":[url],"relay_root_certificates":paths});
        self.save();
    }
}
fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
pub fn profile() -> Value {
    json!({"team_alpn":[102,110,112,116],"join_alpn":[102,110,112,106],"protocol":19,
        "signing_prefix":"process-fixture-signature/","invitation_endpoint_context":"process-fixture-endpoint",
        "invitation_secret_context":"process-fixture-bearer","ticket_prefixes":["process19"]})
}
