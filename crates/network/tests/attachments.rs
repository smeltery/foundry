#![cfg(unix)]
use foundry_network::{attachments::*, wire::TextAttachment};
#[path = "attachments/support.rs"]
mod support;
use std::{fs, path::PathBuf};
use support::*;

#[test]
fn explicit_workspace_reads_text_and_strips_local_source_paths() {
    let root = Root::new();
    root.write("note.md", "Hello λ\n\tend\r\n");
    let files = prepare(&root.policy(), &[PathBuf::from("note.md")]).unwrap();
    assert_eq!(files, vec![file("note.md", "Hello λ\n\tend\r\n")]);
    assert!(files[0].path.is_none());
}
#[test]
fn sender_checks_workspace_exclusions_and_allows_safe_symlink_targets() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    let outside = Root::new();
    root.mkdir("permitted");
    root.mkdir("private");
    root.write("permitted/f.txt", "safe");
    root.write("private/f.txt", "do not send");
    outside.write("other.txt", "outside");
    let policy = SenderPolicy::new(
        &root.0,
        &[root.0.join("permitted")],
        &[root.0.join("private")],
    )
    .unwrap();
    assert!(prepare(&policy, &["permitted/f.txt".into()]).is_ok());
    let denied = SenderPolicy::new(
        &root.0,
        std::slice::from_ref(&root.0),
        &[root.0.join("private")],
    )
    .unwrap();
    assert!(prepare(&denied, &["private/f.txt".into()]).is_err());
    for path in [
        root.0.join("private/f.txt"),
        outside.0.join("other.txt"),
        PathBuf::from("../other.txt"),
    ] {
        assert!(prepare(&policy, &[path]).is_err());
    }
    symlink(&outside.0, root.0.join("permitted/alias")).unwrap();
    symlink(
        root.0.join("permitted/f.txt"),
        root.0.join("permitted/link.txt"),
    )
    .unwrap();
    assert!(prepare(&policy, &["permitted/alias/other.txt".into()]).is_err());
    let safe = prepare(
        &policy,
        &["permitted/f.txt".into(), "permitted/link.txt".into()],
    )
    .unwrap();
    assert_eq!(
        safe.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["f.txt", "f-2.txt"]
    );
    fs::hard_link(
        root.0.join("permitted/f.txt"),
        root.0.join("permitted/second.txt"),
    )
    .unwrap();
    assert_eq!(
        prepare(&policy, &["permitted/second.txt".into()]).unwrap()[0],
        file("second.txt", "safe")
    );
}
#[test]
fn sender_rejects_hidden_secret_nonregular_and_nontext_inputs() {
    let root = Root::new();
    root.mkdir(".hidden");
    root.mkdir("folder");
    for name in [
        ".hidden/note.txt",
        ".env",
        "id_rsa",
        "credentials.json",
        "secret.key",
    ] {
        root.write(name, "text");
    }
    let denied = [
        root.0.join("id_rsa"),
        root.0.join("credentials.json"),
        root.0.join("secret.key"),
    ];
    let guarded = SenderPolicy::new(&root.0, std::slice::from_ref(&root.0), &denied).unwrap();
    for name in [
        ".hidden/note.txt",
        ".env",
        "id_rsa",
        "credentials.json",
        "secret.key",
        "folder",
    ] {
        assert!(prepare(&guarded, &[name.into()]).is_err(), "{name}");
    }
    for (i, bytes) in [
        b"invalid\xff".as_slice(),
        b"zero\0",
        b"escape\x1b",
        "c1\u{0085}".as_bytes(),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("bad{i}.txt");
        fs::write(root.0.join(&name), bytes).unwrap();
        assert!(prepare(&root.policy(), &[name.into()]).is_err());
    }
    root.write("note.txt", "-----BEGIN PRIVATE KEY-----\n");
    assert!(prepare(&root.policy(), &["note.txt".into()]).is_ok());
}
#[test]
fn sender_and_peer_independently_enforce_exact_byte_count_and_case_limits() {
    let root = Root::new();
    let content = "é".repeat(MAX_FILE_BYTES / 2);
    root.write("one.txt", &content);
    root.write("two.txt", &content);
    let files = prepare(&root.policy(), &["one.txt".into(), "two.txt".into()]).unwrap();
    assert_eq!(validate_received(&files).unwrap(), MAX_TOTAL_BYTES);
    root.write("over.txt", &(content.clone() + "x"));
    assert_eq!(
        prepare(&root.policy(), &["over.txt".into()]).unwrap_err(),
        AttachmentError::Limit
    );
    let mut three = files.clone();
    three.push(file("three.txt", "x"));
    assert_eq!(validate_received(&three), Err(AttachmentError::Limit));
    assert_eq!(
        normalize_received(&[file("Note.txt", "a"), file("note.txt", "b")]).unwrap()[1].name,
        "note-2.txt"
    );
    let ten: Vec<_> = (0..10).map(|i| file(&format!("n{i}.txt"), "")).collect();
    assert!(validate_received(&ten).is_ok());
    let mut eleven = ten;
    eleven.push(file("extra.txt", ""));
    assert_eq!(validate_received(&eleven), Err(AttachmentError::Limit));
}
#[test]
fn peer_normalization_never_trusts_a_filename_or_local_path() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    for (i, name) in [
        "../escape.txt",
        "/escape.txt",
        "nested/name.txt",
        "a\\b",
        "C:note",
        ".env",
        "-flag",
        "NUL.txt",
        "COM1",
        "trailing.",
        "zero\0",
        "bidi\u{202e}.txt",
        "élan.txt",
    ]
    .iter()
    .enumerate()
    {
        let saved = store
            .persist(&key(&format!("normal-{i}")), &[file(name, "text")], 0)
            .unwrap();
        assert_eq!(saved[0].name, safe_name(name));
        let path = PathBuf::from(saved[0].path.as_ref().unwrap());
        assert_eq!(path.parent().unwrap().parent().unwrap(), root.0);
        assert_eq!(fs::read_to_string(path).unwrap(), "text");
    }
    let mut forged = file("plain.txt", "x");
    forged.path = Some("/elsewhere/target".into());
    let located = store.persist(&key("forged"), &[forged], 0).unwrap();
    assert!(
        located[0]
            .path
            .as_ref()
            .unwrap()
            .starts_with(root.0.to_str().unwrap())
    );
    let before = store.usage().unwrap();
    assert_eq!(
        store
            .persist(&key("bad-text"), &[file("plain.txt", "a\0b")], 0)
            .unwrap_err(),
        AttachmentError::InvalidText
    );
    assert_eq!(store.usage().unwrap(), before);
}
#[test]
fn persistent_recipient_owned_files_are_private_nonexecutable_and_exact_retry_safe() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = Root::new();
    let files = vec![file("script.sh", "echo sample\n")];
    let owner = key("m/../../one");
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let located = store.persist(&owner, &files, 5).unwrap();
    let path = PathBuf::from(located[0].path.as_ref().unwrap());
    assert!(path.starts_with(&root.0));
    assert_eq!(located[0].content, "");
    assert_eq!(fs::read_to_string(&path).unwrap(), files[0].content);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(store.persist(&owner, &files, 10).unwrap(), located);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(
        store
            .persist(&owner, &[file("script.sh", "changed")], 10)
            .unwrap_err(),
        AttachmentError::Conflict
    );
    let other = AttachmentKey::new("sender", "another-recipient", &owner.message).unwrap();
    assert_ne!(
        store.persist(&other, &files, 10).unwrap()[0].path,
        located[0].path
    );
    drop(store);
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert_eq!(store.recover(&owner).unwrap(), located);
    assert_eq!(store.load_payload(&owner).unwrap(), files);
    fs::remove_file(&path).unwrap();
    assert_eq!(store.recover(&owner).unwrap(), located);
    assert_eq!(fs::read_to_string(path).unwrap(), files[0].content);
}
#[path = "attachments/quota.rs"]
mod quota;
#[path = "attachments/recovery.rs"]
mod recovery;

#[path = "attachments/expiration.rs"]
mod expiration;
#[path = "attachments/names.rs"]
mod names;
#[path = "attachments/sender_modes.rs"]
mod sender_modes;
