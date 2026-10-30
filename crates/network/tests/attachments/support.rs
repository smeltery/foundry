use super::*;
use std::os::unix::fs::PermissionsExt;
pub struct Root(pub PathBuf);
impl Root {
    pub fn new() -> Self {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).unwrap();
        let path = PathBuf::from("/tmp")
            .join(format!("attach-{}", data_encoding::HEXLOWER.encode(&nonce)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub fn mkdir(&self, path: &str) {
        fs::create_dir(self.0.join(path)).unwrap();
    }
    pub fn write(&self, path: &str, content: &str) {
        fs::write(self.0.join(path), content).unwrap();
    }
    pub fn policy(&self) -> SenderPolicy {
        SenderPolicy::new(&self.0, std::slice::from_ref(&self.0), &[]).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
pub fn file(name: &str, content: &str) -> TextAttachment {
    TextAttachment {
        name: name.into(),
        content: content.into(),
        path: None,
    }
}
pub fn key(message: &str) -> AttachmentKey {
    AttachmentKey::new("sender", "recipient", message).unwrap()
}
pub fn directory(key: &AttachmentKey) -> String {
    let mut hash = blake3::Hasher::new_derive_key("foundry attachment ownership v1");
    for v in [&key.sender, &key.recipient, &key.message] {
        hash.update(&(v.len() as u64).to_be_bytes());
        hash.update(v.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}
pub fn private_file(root: &Root, path: &str, content: &str) {
    root.write(path, content);
    fs::set_permissions(root.0.join(path), fs::Permissions::from_mode(0o600)).unwrap();
}
