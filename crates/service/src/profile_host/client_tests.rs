use super::{Error, Registration, RegistrationOptions, StoredProfile, client::Operation};
use foundry_contracts::context::{ContextDigest, ContextFormat, ContextId};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

fn registration(directory: &std::path::Path) -> Registration {
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let path = directory.join("host");
    std::fs::write(
        &path,
        format!(
            "#!{}\n{}",
            String::from_utf8(python.stdout).unwrap().trim(),
            include_str!("client_fixture.py")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let id = || ContextId::new("fixture").unwrap();
    Registration::new(RegistrationOptions {
        executable: path,
        stored: StoredProfile {
            format_id: "fixture".into(),
            format_version: 1,
            profile_id: "fixture".into(),
            binding_id: "fixture".into(),
            binding_version: "1".into(),
        },
        wire: ContextFormat {
            profile: id(),
            format_version: id(),
            producer_build: id(),
        },
        validator_id: id(),
        validator_version: id(),
        observation_profile_digest: ContextDigest::of(b"profile"),
        registration_digest: ContextDigest::of(b"registration"),
        configuration: b"{}".to_vec(),
    })
    .unwrap()
}

#[test]
fn python_hashes_exact_request_and_reply_correlation_is_mandatory() {
    let directory = tempfile::tempdir().unwrap();
    let registration = registration(directory.path());
    let call = |mode| {
        registration.call::<Value>(
            Operation::Configure,
            &json!({"mode":mode,"text":"quoted \" / 🦀"}),
            Instant::now() + Duration::from_secs(3),
            || false,
        )
    };
    assert_eq!(call("ok").unwrap(), json!({"accepted":true}));
    for mode in [
        "digest",
        "identity",
        "operation",
        "unknown",
        "duplicate",
        "trailing",
        "stderr",
        "oversize",
    ] {
        assert!(call(mode).is_err(), "{mode}");
    }
}

#[test]
fn inherited_output_cannot_outlive_host_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let registration = registration(directory.path());
    let start = Instant::now();
    let result = registration.call::<Value>(
        Operation::Configure,
        &json!({"mode":"descendant"}),
        start + Duration::from_millis(150),
        || false,
    );
    assert!(matches!(result, Err(Error::Deadline)));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn cancelled_or_expired_requests_do_not_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let registration = registration(directory.path());
    std::fs::remove_file(&registration.options.executable).unwrap();
    let input = json!({"mode":"ok"});
    assert!(matches!(
        registration.call::<Value>(
            Operation::Configure,
            &input,
            Instant::now() + Duration::from_secs(1),
            || true
        ),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        registration.call::<Value>(Operation::Configure, &input, Instant::now(), || false),
        Err(Error::Deadline)
    ));
}
