use super::*;
use cap_std::ambient_authority;
use std::{fs, io::Read, os::unix::fs::PermissionsExt, path::PathBuf};

pub(crate) struct Root(pub PathBuf);
impl Root {
    pub fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "reply-test-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub fn directory(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, ambient_authority()).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn private_spool_is_unlinked_bounded_and_immutable_after_sealing() {
    let root = Root::new();
    let mut spool = Spool::create(&root.directory(), 8).unwrap();
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    let metadata = spool.file.metadata().unwrap();
    assert_eq!(metadata.nlink(), 0);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    spool.write_all(b"payload\n").unwrap();
    assert!(spool.write_all(b"x").is_err());
    spool.seal().unwrap();
    assert!(spool.write_all(b"x").is_err());
    let mut file = spool.into_file(8).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"payload\n");
}
#[test]
fn unpublished_or_mismatched_spool_cannot_be_promoted() {
    let root = Root::new();
    assert!(
        Spool::create(&root.directory(), 8)
            .unwrap()
            .into_file(0)
            .is_err()
    );
    let mut spool = Spool::create(&root.directory(), 8).unwrap();
    spool.write_all(b"known").unwrap();
    // A local I/O inconsistency must fail seal before a caller can commit.
    spool.file.set_len(2).unwrap();
    assert!(spool.seal().is_err());
    assert!(spool.into_file(5).is_err());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}
#[test]
fn unprotected_root_is_refused_without_creating_an_entry() {
    let root = Root::new();
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Spool::create(&root.directory(), 8).is_err());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}
