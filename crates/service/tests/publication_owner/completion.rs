//! foundry.completion.v1 on native publication: the checkpoint keeps the
//! adapter's terminal proposal, the session stores the decision bound to it.
use crate::*;
use foundry_contracts::{CompletionCheck, CompletionCriteria, Lifecycle, Outcome};

fn publish(
    notes: bool,
) -> (
    tempfile::TempDir,
    Store,
    String,
    foundry_store::FinalizedCheckpoint,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    if notes {
        std::fs::write(dir.path().join("notes.txt"), "notes").unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "native completion".into(),
            directory: dir.path().display().to_string(),
            completion: Some(CompletionCriteria {
                profile: None,
                checks: vec![CompletionCheck::File {
                    id: "notes".into(),
                    path: "notes.txt".into(),
                    sha256: None,
                }],
            }),
            ..Default::default()
        })
        .unwrap();
    let mut owner = start_for_test(path, session.id.clone(), configuration());
    let FreshCaptureReply::Ready(ports) = receive(&mut owner) else {
        panic!()
    };
    let cancellation = owner.take_cancellation().unwrap();
    let (_send, messages) = mpsc::channel();
    let mut spec = AdapterSpec::new("/usr/bin/python3");
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("../context_capture/adapter.py").into(),
        dir.path().as_os_str().into(),
        "\"fixture terminal\"".into(),
        "success".into(),
    ];
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(100);
    let options = FreshCaptureOptions {
        profile: ports.profile,
        owner: ports.owner,
        policy: None,
        phase_timeout: Duration::from_secs(3),
    };
    let result = run_fresh_capture(
        &spec,
        &ports.session,
        &ports.attempt_id,
        &cancellation,
        &messages,
        ports.persistence,
        options,
    )
    .unwrap();
    let CaptureExit::Prepared(prepared) = result else {
        panic!()
    };
    owner.finish_capture(*prepared).unwrap();
    let FreshCaptureReply::Published(finalized) = receive(&mut owner) else {
        panic!()
    };
    let report = owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();

    (dir, store, session.id, *finalized)
}

#[test]
fn native_success_requires_checks_run_before_publication() {
    for (notes, outcome, cause) in [
        (true, Outcome::Success, "criteria_verified"),
        (false, Outcome::Failure, "criterion_failed"),
    ] {
        let (_dir, store, session, finalized) = publish(notes);
        assert_eq!(finalized.terminal.outcome, Some(Outcome::Success));
        let stored = store.get(&session).unwrap();
        assert_eq!(stored.lifecycle, Lifecycle::Exit);
        assert_eq!(stored.outcome, Some(outcome));
        let decision = store
            .current_completion_decision(&session)
            .unwrap()
            .unwrap();
        assert_eq!(decision.cause, cause);
        assert_eq!(
            decision.checkpoint_id.as_deref(),
            Some(finalized.checkpoint.id.as_str())
        );
        assert_eq!(
            decision.configuration_digest,
            std::array::from_fn(|i| i as u8)
        );
    }
}
