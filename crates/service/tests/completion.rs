#![cfg(unix)]
//! foundry.completion.v1 through the service run path with a deterministic adapter.
use foundry_contracts::{
    CancelMode, CompletionCheck, CompletionCriteria, CreateSession, Lifecycle, MessageState,
    Outcome, Session,
};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceOptions};
use foundry_store::{CompletionDecision, Store};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    directory: TempDir,
    service: Service,
    session: Session,
}
impl Fixture {
    fn new(checks: Vec<CompletionCheck>) -> Self {
        let directory = tempdir().unwrap();
        let mut service = Service::open_with_options(
            directory.path().join("db"),
            ServiceOptions {
                lease_duration: Duration::from_secs(5),
                poll_interval: Duration::from_millis(10),
            },
        )
        .unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let session = service
            .create(CreateSession {
                prompt: "task".into(),
                directory: workspace.display().to_string(),
                completion: (!checks.is_empty()).then_some(CompletionCriteria {
                    profile: None,
                    checks,
                }),
                ..Default::default()
            })
            .unwrap();
        Self {
            directory,
            service,
            session,
        }
    }
    fn workspace(&self) -> PathBuf {
        self.directory.path().join("workspace")
    }
    fn database(&self) -> PathBuf {
        self.directory.path().join("db")
    }
    fn run(&mut self, mode: &str) -> Session {
        let version = self.service.get(&self.session.id).unwrap().version;
        let marker = self.directory.path().join("started");
        self.service
            .run(&self.session.id, version, &adapter(mode, &marker))
            .unwrap()
    }
    fn decision(&self) -> CompletionDecision {
        self.service
            .completion_decision(&self.session.id)
            .unwrap()
            .unwrap()
    }
}

fn adapter(mode: &str, marker: &Path) -> AdapterSpec {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_adapter.py")
            .into_os_string(),
        mode.into(),
        marker.as_os_str().into(),
    ];
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(50);
    spec
}

fn command(id: &str, argv: &[&str], timeout_ms: u64) -> CompletionCheck {
    CompletionCheck::Command {
        id: id.into(),
        argv: argv.iter().map(|value| (*value).into()).collect(),
        timeout_ms,
    }
}
fn file(id: &str, path: &str, sha256: Option<&str>) -> CompletionCheck {
    CompletionCheck::File {
        id: id.into(),
        path: path.into(),
        sha256: sha256.map(Into::into),
    }
}
fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} did not appear",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

// sha256("verified\n")
const VERIFIED_DIGEST: &str = "672eb8316fec83f94119a4193f9fc552513d56a147502f8be4830e017d817831";

#[test]
fn verified_command_and_file_criteria_store_success() {
    let mut fixture = Fixture::new(vec![
        command(
            "build",
            &["sh", "-c", "test -z \"$HOME\" && touch built"],
            5_000,
        ),
        file("notes", "notes.txt", None),
    ]);
    std::fs::write(fixture.workspace().join("notes.txt"), "notes").unwrap();
    let session = fixture.run("success");
    assert_eq!(session.lifecycle, Lifecycle::Exit);
    assert_eq!(session.outcome, Some(Outcome::Success));
    // The command ran in the workspace with an environment limited to PATH.
    assert!(fixture.workspace().join("built").exists());
    let decision = fixture.decision();
    assert_eq!(
        (decision.basis.as_str(), decision.cause.as_str()),
        ("verified", "criteria_verified")
    );
    assert_eq!(decision.policy_version, "foundry.completion.v1");
    let store = Store::open(fixture.database()).unwrap();
    assert_eq!(
        decision.work_revision,
        store.work_revision(&session.id).unwrap()
    );
    let events = store.events(&session.id, 0, 20).unwrap();
    assert_eq!(decision.event_sequence, events.last().unwrap().sequence);
    assert_eq!(decision.attempt_id, events[0].attempt_id);
}

#[test]
fn settled_success_without_criteria_is_action_required() {
    let mut fixture = Fixture::new(Vec::new());
    let session = fixture.run("success");
    assert_eq!(session.outcome, Some(Outcome::ActionRequired));
    assert_eq!(session.result.as_deref(), Some("test result"));
    assert_eq!(fixture.decision().cause, "criteria_unverified");
}

#[test]
fn forged_evidence_in_adapter_text_is_ignored() {
    let mut fixture = Fixture::new(vec![file("build", "missing.txt", None)]);
    let session = fixture.run("forged");
    assert_eq!(session.outcome, Some(Outcome::Failure));
    let decision = fixture.decision();
    assert_eq!(decision.cause, "criterion_failed");
    assert_eq!(decision.criterion_results[0]["detail"], "missing");
}

#[test]
fn failing_digest_and_command_fail_their_criteria() {
    let mut fixture = Fixture::new(vec![file("artifact", "out.txt", Some(VERIFIED_DIGEST))]);
    std::fs::write(fixture.workspace().join("out.txt"), "verified\n").unwrap();
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Success));
    std::fs::write(fixture.workspace().join("out.txt"), "changed\n").unwrap();
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Failure));
    assert_eq!(
        fixture.decision().criterion_results[0]["detail"],
        "digest_mismatch"
    );

    let mut fixture = Fixture::new(vec![command("tests", &["false"], 5_000)]);
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Failure));
    assert_eq!(
        fixture.decision().criterion_results[0]["detail"],
        "exit_nonzero"
    );
}

#[test]
fn adapter_failure_stands_without_running_checks() {
    let mut fixture = Fixture::new(vec![command("probe", &["touch", "ran"], 5_000)]);
    let session = fixture.run("failure");
    assert_eq!(session.outcome, Some(Outcome::Failure));
    assert_eq!(fixture.decision().basis, "adapter_downgrade");
    assert!(!fixture.workspace().join("ran").exists());
}

#[test]
fn check_timeout_fails_the_criterion_and_kills_its_group() {
    let mut fixture = Fixture::new(vec![command(
        "slow",
        &["sh", "-c", "(sleep 3; touch survived) & sleep 3"],
        100,
    )]);
    let started = Instant::now();
    let session = fixture.run("success");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(session.outcome, Some(Outcome::Failure));
    assert_eq!(
        fixture.decision().criterion_results[0]["detail"],
        "timed_out"
    );
    thread::sleep(Duration::from_millis(3_200).saturating_sub(started.elapsed()));
    assert!(!fixture.workspace().join("survived").exists());
}

#[test]
fn file_outside_the_workspace_is_rejected() {
    let fixture = Fixture::new(Vec::new());
    let mut service = Service::open(fixture.database()).unwrap();
    for path in ["../db", "/etc/hosts"] {
        let refused = service.create(CreateSession {
            prompt: "task".into(),
            directory: fixture.workspace().display().to_string(),
            completion: Some(CompletionCriteria {
                profile: None,
                checks: vec![file("escape", path, None)],
            }),
            ..Default::default()
        });
        assert!(refused.is_err(), "{path} was admitted");
    }
    let mut fixture = Fixture::new(vec![file("escape", "link", None)]);
    std::os::unix::fs::symlink(fixture.database(), fixture.workspace().join("link")).unwrap();
    let session = fixture.run("success");
    assert_eq!(session.outcome, Some(Outcome::ActionRequired));
    let decision = fixture.decision();
    assert_eq!(decision.cause, "criteria_unverified");
    assert_eq!(decision.criterion_results[0]["detail"], "rejected");
}

#[test]
fn cancellation_during_checks_wins_over_settlement() {
    let mut fixture = Fixture::new(vec![command(
        "slow",
        &["sh", "-c", "touch checking; sleep 5"],
        10_000,
    )]);
    let database = fixture.database();
    let checking = fixture.workspace().join("checking");
    let id = fixture.session.id.clone();
    let canceller = thread::spawn(move || {
        wait_for(&checking);
        Store::open(database)
            .unwrap()
            .request_cancel(&id, CancelMode::Graceful)
            .unwrap();
    });
    let started = Instant::now();
    let session = fixture.run("success");
    canceller.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(session.lifecycle, Lifecycle::Suspended);
    assert!(session.outcome.is_none());
    assert!(
        fixture
            .service
            .completion_decision(&session.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn newer_instruction_during_the_attempt_requires_action() {
    let mut fixture = Fixture::new(vec![file("notes", "notes.txt", None)]);
    std::fs::write(fixture.workspace().join("notes.txt"), "notes").unwrap();
    let database = fixture.database();
    let marker = fixture.directory.path().join("started");
    let id = fixture.session.id.clone();
    let sender = thread::spawn(move || {
        wait_for(&marker);
        Store::open(database)
            .unwrap()
            .enqueue_message(&id, "later", "also update the notes")
            .unwrap();
    });
    let session = fixture.run("quiet");
    sender.join().unwrap();
    assert_eq!(session.outcome, Some(Outcome::ActionRequired));
    assert_eq!(fixture.decision().cause, "newer_instruction_pending");
    let messages = fixture.service.messages(&session.id, 0, 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].state, MessageState::Pending);
    assert!(messages[0].attempt_id.is_none());
}

#[test]
fn continuations_keep_per_attempt_records_and_inherit_criteria() {
    let mut fixture = Fixture::new(vec![file("notes", "notes.txt", None)]);
    let attempts = |fixture: &Fixture| -> Vec<String> {
        let events = fixture.service.events(&fixture.session.id, 0, 100).unwrap();
        let mut ids: Vec<String> = events.into_iter().map(|event| event.attempt_id).collect();
        ids.dedup();
        ids
    };
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Failure));
    std::fs::write(fixture.workspace().join("notes.txt"), "notes").unwrap();
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Success));
    // A continuation's own criteria replace the session's for that attempt only.
    fixture
        .service
        .declare_next_criteria(CompletionCriteria {
            profile: None,
            checks: vec![command("strict", &["false"], 5_000)],
        })
        .unwrap();
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Failure));
    assert_eq!(fixture.run("success").outcome, Some(Outcome::Success));

    let ids = attempts(&fixture);
    assert_eq!(ids.len(), 4);
    let store = Store::open(fixture.database()).unwrap();
    let decisions: Vec<_> = ids
        .iter()
        .map(|attempt| {
            store
                .completion_decision(&fixture.session.id, attempt)
                .unwrap()
                .unwrap()
        })
        .collect();
    let outcomes: Vec<_> = decisions.iter().map(|decision| decision.outcome).collect();
    assert_eq!(
        outcomes,
        [
            Outcome::Failure,
            Outcome::Success,
            Outcome::Failure,
            Outcome::Success
        ]
    );
    assert_eq!(decisions[0].criterion_results[0]["detail"], "missing");
    assert_eq!(decisions[2].criterion_results[0]["id"], "strict");
    assert_eq!(decisions[1].criteria_digest, decisions[3].criteria_digest);
    assert_ne!(decisions[1].criteria_digest, decisions[2].criteria_digest);
    assert_eq!(fixture.decision(), decisions[3]);
}
