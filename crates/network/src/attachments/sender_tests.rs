use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
};
struct Sandbox(std::path::PathBuf);
impl Sandbox {
    fn new() -> Self {
        let mut random = [0; 12];
        getrandom::fill(&mut random).unwrap();
        let path = std::path::PathBuf::from("/tmp").join(format!(
            "attach-race-{}",
            data_encoding::HEXLOWER.encode(&random)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn policy(&self) -> SenderPolicy {
        SenderPolicy::new(&self.0, std::slice::from_ref(&self.0), &[]).unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn retargeted_selection_alias_cannot_return_an_outside_payload() {
    let root = Sandbox::new();
    let outside = Sandbox::new();
    fs::write(root.0.join("inside.txt"), "inside").unwrap();
    fs::write(outside.0.join("outside.txt"), "outside").unwrap();
    let alias = root.0.join("alias.txt");
    symlink(root.0.join("inside.txt"), &alias).unwrap();
    let result = prepare_with(&root.policy(), Path::new("alias.txt"), |canonical| {
        assert_eq!(canonical, root.0.join("inside.txt"));
        fs::remove_file(&alias).unwrap();
        symlink(outside.0.join("outside.txt"), &alias).unwrap();
    });
    assert!(matches!(result, Err(AttachmentError::UnsafePath)));
}
#[test]
fn canonical_target_or_directory_replaced_by_symlink_is_rejected_before_read() {
    for directory in [false, true] {
        let root = Sandbox::new();
        let outside = Sandbox::new();
        fs::create_dir(root.0.join("docs")).unwrap();
        fs::write(root.0.join("docs/file.txt"), "inside").unwrap();
        fs::write(outside.0.join("file.txt"), "outside").unwrap();
        let result = prepare_with(&root.policy(), Path::new("docs/file.txt"), |_| {
            if directory {
                fs::rename(root.0.join("docs"), root.0.join("saved")).unwrap();
                symlink(&outside.0, root.0.join("docs")).unwrap();
            } else {
                fs::remove_file(root.0.join("docs/file.txt")).unwrap();
                symlink(outside.0.join("file.txt"), root.0.join("docs/file.txt")).unwrap();
            }
        });
        assert!(matches!(result, Err(AttachmentError::UnsafePath)));
    }
}
