use super::*;
use crate::bootstrap::{BootstrapIdentity, PrivateBootstrap};
use foundry_contracts::{Lifecycle, context::ContextDigest};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

struct Fixture {
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("f5y-bootstrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }
    fn session(&self) -> Session {
        Session {
            id: "s".into(),
            prompt: "task".into(),
            directory: self.directory.to_string_lossy().into(),
            model: None,
            lifecycle: Lifecycle::Running,
            outcome: None,
            result: None,
            active_attempt_id: Some("a".into()),
            version: 1,
        }
    }
    fn spec(&self, mode: &str, body: Vec<u8>) -> AdapterSpec {
        let python = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
        spec.args = vec![
            "-u".into(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/bootstrap_fixture.py")
                .into_os_string(),
            mode.into(),
        ];
        spec.ready_timeout = Duration::from_secs(2);
        spec.with_private_bootstrap(
            PrivateBootstrap::new(
                BootstrapIdentity {
                    schema_version: 1,
                    selection_digest: ContextDigest::of(b"selection"),
                },
                body,
            )
            .unwrap(),
        )
    }
    fn run(&self, spec: &AdapterSpec) -> Result<crate::RunResult, RuntimeError> {
        let (_cancel, cancel) = mpsc::channel();
        let (_messages, messages) = mpsc::channel();
        crate::run_adapter_with_messages(spec, &self.session(), "a", &cancel, &messages, |_| {})
    }
    fn cleaned(&self) {
        let pid = self.directory.join("pid");
        if pid.exists() {
            let pid: i32 = std::fs::read_to_string(pid).unwrap().parse().unwrap();
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn private_bootstrap_preserves_body_and_is_spent_across_spec_clones() {
    for body in [
        b"private-canary-unique".to_vec(),
        (0..65536).map(|i| (i % 256) as u8).collect(),
    ] {
        let f = Fixture::new();
        let spec = f.spec("stderr", body.clone());
        let clone = spec.clone();
        assert!(!format!("{spec:?}").contains("private-canary-unique"));
        assert!(matches!(
            f.run(&spec),
            Ok(crate::RunResult::Completed { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(f.directory.join("received")).unwrap(),
            ContextDigest::of(&body).to_hex()
        );
        assert!(matches!(
            f.run(&clone),
            Err(RuntimeError::Invalid("private bootstrap already consumed"))
        ));
        f.cleaned();
    }
}
#[test]
fn invalid_bootstrap_is_rejected_and_failed_spawn_cannot_replay() {
    for (version, body) in [(2, vec![1]), (1, vec![]), (1, vec![0; 65537])] {
        assert!(
            PrivateBootstrap::new(
                BootstrapIdentity {
                    schema_version: version,
                    selection_digest: ContextDigest::of(b"x")
                },
                body
            )
            .is_err()
        );
    }
    let f = Fixture::new();
    let mut spec = f.spec("normal", vec![1]);
    spec.executable = "/missing/adapter".into();
    assert!(f.run(&spec).is_err());
    assert!(matches!(
        f.run(&spec),
        Err(RuntimeError::Invalid("private bootstrap already consumed"))
    ));
}
#[test]
fn invalid_or_missing_configuration_never_sends_start() {
    for mode in [
        "early",
        "wrong",
        "version",
        "selection",
        "extra",
        "duplicate_field",
        "oversize",
        "duplicate",
    ] {
        let f = Fixture::new();
        let spec = f.spec(mode, b"private-canary-unique".to_vec());
        let error = f.run(&spec).unwrap_err();
        assert!(
            !error.to_string().contains("private-canary-unique"),
            "{mode}"
        );
        assert!(!f.directory.join("started").exists(), "{mode}");
        f.cleaned();
    }
}
#[test]
fn blocked_writes_and_missing_ready_keep_original_deadline() {
    for mode in ["blocked", "stall", "partial", "no_ready", "delayed"] {
        let f = Fixture::new();
        let mut spec = f.spec(mode, vec![b'x'; 65536]);
        spec.ready_timeout = Duration::from_millis(450);
        let now = Instant::now();
        assert!(
            matches!(f.run(&spec), Err(RuntimeError::ReadyTimeout)),
            "{mode}"
        );
        assert!(now.elapsed() < Duration::from_secs(2));
        assert!(!f.directory.join("started").exists());
        f.cleaned();
    }
}
#[test]
fn cancellation_interrupts_private_bootstrap_and_joins_workers() {
    let f = Fixture::new();
    let spec = f.spec("blocked", vec![b'x'; 65536]);
    let (sender, cancel) = mpsc::channel();
    let (_messages, messages) = mpsc::channel();
    thread::scope(|scope| {
        let fixture = &f;
        let spec = &spec;
        let worker = scope.spawn(move || {
            crate::run_adapter_with_messages(
                spec,
                &fixture.session(),
                "a",
                &cancel,
                &messages,
                |_| {},
            )
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !f.directory.join("pid").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        sender.send(foundry_contracts::CancelMode::Hard).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            Ok(crate::RunResult::Cancelled { .. })
        ));
    });
    assert!(!f.directory.join("started").exists());
    f.cleaned();
}

#[test]
fn queued_commands_cannot_bypass_ack_and_private_ack_never_becomes_event() {
    let f = Fixture::new();
    let spec = f.spec("wrong", vec![1]);
    let mut process = Process::spawn(&spec, &f.session()).unwrap();
    process
        .send(&AdapterCommand::Start {
            version: foundry_contracts::PROTOCOL_VERSION,
            attempt_id: "a".into(),
            session: f.session(),
        })
        .unwrap();
    assert!(matches!(
        process.events.recv_timeout(Duration::from_secs(3)).unwrap(),
        WireEvent::Error(_)
    ));
    process.stop().unwrap();
    assert!(!f.directory.join("started").exists());
    f.cleaned();
    let f = Fixture::new();
    let spec = f.spec("normal", vec![1]);
    let (mut process, events) = Process::spawn_context(&spec, &f.session()).unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(3)).unwrap(),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    process.stop().unwrap();
    f.cleaned();
}
