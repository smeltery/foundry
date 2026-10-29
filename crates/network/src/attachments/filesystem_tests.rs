use super::*;
use std::{fs, os::unix::fs::DirBuilderExt};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "attachment-sync-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn directory_sync_uses_opened_owner_after_its_path_is_replaced() {
    let fixture = Sandbox::new();
    let path = fixture.0.join("receiver");
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
    let moved = fixture.0.join("retained");
    fs::rename(&path, &moved).unwrap();
    // A file at the former pathname makes any ambient reopen fail deterministically.
    fs::write(&path, "replacement must be preserved").unwrap();
    let nested = directory(&root, "batch", true).unwrap();
    let payload = create_file(&nested, "payload.txt").unwrap();
    payload.sync_all().unwrap();
    for (name, dir) in [("receiver", &root), ("batch", &nested)] {
        let descriptor = dir.try_clone().unwrap().into_std_file();
        let result = descriptor.sync_all();
        eprintln!("{name} cloned directory fsync diagnostic: {result:?}");
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            eprintln!("{name} descriptor O_PATH: {}", flags & libc::O_PATH != 0);
        }
        sync(dir).expect("directory durability must succeed on a retained capability");
    }
    assert!(moved.join("batch/payload.txt").is_file());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "replacement must be preserved"
    );
}
