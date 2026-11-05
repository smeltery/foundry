//! Secret handling: redaction, purge on delete, the token reader and the
//! private-input handoff.

#[path = "credentials_support.rs"]
mod support;

use foundry_identity::authz::Role;
use foundry_providers::credentials::{
    CredentialError, FixtureValidator, MAX_SECRET_BYTES, Secret, read_token,
};
use foundry_service::prepared_adapter::AdapterCredential;
use std::io::Cursor;
use std::path::Path;
use support::*;

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Every file SQLite may keep for the credential database.
fn on_disk(directory: &Path, needle: &str) -> bool {
    let base = path(directory);
    ["", "-wal", "-shm", "-journal"].iter().any(|suffix| {
        let file = format!("{}{suffix}", base.display());
        std::fs::read(file).is_ok_and(|bytes| contains(&bytes, needle))
    })
}

#[test]
fn canary_never_appears_in_debug_display_or_errors() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let mut rendered = vec![format!("{:?}", secret(CANARY))];
    rendered.push(format!(
        "{:?}",
        request("hosted-a", None, CANARY, &[("region", "north")])
    ));
    let configured = store
        .configure(&owner(), &catalog(), request("hosted-a", None, CANARY, &[]))
        .unwrap();
    rendered.push(format!("{configured:?}"));
    rendered.push(format!("{:?}", store.list(&owner()).unwrap()));
    let handoff = store
        .handoff(&owner(), &id("hosted-a"), &account("account-1"))
        .unwrap();
    rendered.push(format!("{handoff:?}"));
    let check = store.check_account(
        &owner(),
        &catalog(),
        &FixtureValidator { reachable: true },
        &id("hosted-a"),
        &account("account-1"),
    );
    rendered.push(format!("{check:?}"));

    let viewer = caller("workspace-a", "viewer-a", Role::Viewer);
    let mut not_utf8 = CANARY.as_bytes().to_vec();
    not_utf8.push(0xff);
    let errors: Vec<CredentialError> = vec![
        store
            .configure(&owner(), &catalog(), request("hosted-z", None, CANARY, &[]))
            .unwrap_err(),
        store
            .configure(
                &owner(),
                &catalog(),
                request("hosted-a", None, CANARY, &[("x", CANARY)]),
            )
            .unwrap_err(),
        store
            .configure(&viewer, &catalog(), request("hosted-a", None, CANARY, &[]))
            .unwrap_err(),
        Secret::new(format!("{CANARY}{}", "k".repeat(MAX_SECRET_BYTES))).unwrap_err(),
        read_token(&mut Cursor::new(not_utf8)).unwrap_err(),
    ];
    for error in errors {
        rendered.push(format!("{error} {error:?}"));
    }
    for text in &rendered {
        assert!(!text.contains(CANARY), "leaked in {text}");
        assert!(!text.contains("5d1e"), "partial leak in {text}");
    }
}

#[test]
fn delete_and_replace_remove_secret_material_from_disk() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-a", Some("account-1"), CANARY, &[]),
        )
        .unwrap();
    assert!(
        on_disk(directory.path(), CANARY),
        "the scan must see stored material"
    );

    store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-a", Some("account-1"), OTHER_CANARY, &[]),
        )
        .unwrap();
    assert!(
        !on_disk(directory.path(), CANARY),
        "replaced key still on disk"
    );
    assert!(on_disk(directory.path(), OTHER_CANARY));

    let deleted = store.delete(&owner(), &id("hosted-a"), None).unwrap();
    assert!(deleted.secret_purged);
    assert!(
        !on_disk(directory.path(), OTHER_CANARY),
        "deleted key still on disk"
    );
    drop(store);
    assert!(!on_disk(directory.path(), OTHER_CANARY));
}

#[test]
fn secrets_compare_by_value() {
    assert_eq!(secret(CANARY), secret(CANARY));
    assert_ne!(secret(CANARY), secret(OTHER_CANARY));
    assert_ne!(secret(CANARY), secret(&CANARY[1..]));
    assert_eq!(secret(CANARY).expose(), CANARY);
}

#[test]
fn token_reader_takes_one_line_within_limits() {
    let mut input = Cursor::new(b"first-token\r\nsecond-token\n".to_vec());
    assert_eq!(read_token(&mut input).unwrap().expose(), "first-token");
    assert_eq!(read_token(&mut input).unwrap().expose(), "second-token");
    assert_eq!(
        read_token(&mut Cursor::new(b"last".to_vec()))
            .unwrap()
            .expose(),
        "last"
    );
    assert_eq!(
        read_token(&mut Cursor::new(b" padded \n".to_vec()))
            .unwrap()
            .expose(),
        " padded "
    );

    let exact = format!("{}\n", "k".repeat(MAX_SECRET_BYTES));
    assert_eq!(
        read_token(&mut Cursor::new(exact.into_bytes()))
            .unwrap()
            .expose()
            .len(),
        MAX_SECRET_BYTES
    );
    let invalid = |bytes: Vec<u8>| {
        read_token(&mut Cursor::new(bytes))
            .map(|_| ())
            .unwrap_err()
            .class()
    };
    for bytes in [vec![], b"\n".to_vec(), b"\r\n".to_vec(), vec![0xff, b'\n']] {
        assert_eq!(invalid(bytes), "invalid_request");
    }
    assert_eq!(
        invalid(format!("{}\n", "k".repeat(MAX_SECRET_BYTES + 1)).into_bytes()),
        "invalid_request"
    );

    // An endless stream is refused after a bounded read.
    let mut endless = std::io::BufReader::new(std::io::repeat(b'k'));
    assert_eq!(
        read_token(&mut endless).map(|_| ()).unwrap_err().class(),
        "invalid_request"
    );
}

#[cfg(unix)]
#[test]
fn echo_is_off_on_a_terminal_and_restored_afterwards() {
    use foundry_providers::credentials::EchoGuard;
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};

    let (mut controller, mut terminal) = (-1, -1);
    // SAFETY: both out-pointers are valid; the name, settings and size are optional.
    let opened = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0);
    // SAFETY: openpty returned two fresh descriptors owned by this test.
    let (_controller, terminal) = unsafe {
        (
            OwnedFd::from_raw_fd(controller),
            OwnedFd::from_raw_fd(terminal),
        )
    };
    let flags = || {
        let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: the descriptor is live and `settings` is a valid out-pointer.
        assert_eq!(
            unsafe {
                libc::tcgetattr(
                    std::os::fd::AsRawFd::as_raw_fd(&terminal),
                    settings.as_mut_ptr(),
                )
            },
            0
        );
        // SAFETY: tcgetattr succeeded.
        unsafe { settings.assume_init() }.c_lflag
    };
    assert_ne!(flags() & libc::ECHO, 0);
    let guard = EchoGuard::disable(terminal.as_fd())
        .unwrap()
        .expect("a pty is a terminal");
    assert_eq!(flags() & libc::ECHO, 0);
    assert_ne!(flags() & libc::ECHONL, 0);
    drop(guard);
    assert_ne!(flags() & libc::ECHO, 0);

    let file = tempfile::tempfile().unwrap();
    assert!(EchoGuard::disable(file.as_fd()).unwrap().is_none());
}

#[test]
fn handoff_feeds_the_adapter_private_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    store
        .configure(&owner(), &catalog(), request("fixture", None, CANARY, &[]))
        .unwrap();
    let handoff = store
        .handoff(&owner(), &id("fixture"), &account("account-1"))
        .unwrap();
    let (provider, value) = handoff.into_adapter_parts();
    let credential = AdapterCredential::ApiKey { provider, value };
    match credential {
        AdapterCredential::ApiKey { provider, value } => {
            assert_eq!((provider.as_str(), value.as_str()), ("fixture", CANARY));
        }
        AdapterCredential::None => panic!("expected an API key"),
    }
}
