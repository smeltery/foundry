use super::*;
use f5y::Backend;

#[test]
fn failures_are_not_release_unreachable_and_never_launch() {
    for failure in [
        UpdateFailure::PolicyUnresolved,
        UpdateFailure::Unavailable,
        UpdateFailure::Verification,
        UpdateFailure::Other,
    ] {
        let (mut backend, ledger) = fixture(
            snapshot(Scope::Session, Some("1"), Some("1")),
            Err(failure),
            |plan| {
                assert!(plan.update_requested());
                assert!(plan.daily_notice_suppressed());
            },
        );
        let (status, out, err) = run(&["--json", "task"], &mut backend);
        assert_eq!(status, 2);
        assert!(out.is_empty());
        let error: serde_json::Value = serde_json::from_str(&err).unwrap();
        assert_eq!(error["error"]["code"], "configuration");
        assert!(!err.contains("private-"));
        assert_eq!(
            *ledger.borrow(),
            [
                Event::Snapshot(Scope::Session),
                Event::Account,
                Event::Prepare,
                Event::Error(failure)
            ]
        );
        let (_, _, executor) = backend.into_parts();
        assert!(executor.into_parts().2.calls.is_empty());
    }
}

#[test]
fn every_required_sink_failure_stops_without_recursive_diagnostic_or_launch() {
    for (result, expected) in [
        (prepared(PreparationOutcome::Installed, true), Event::Daily),
        (
            prepared(PreparationOutcome::ReleaseServerUnavailable, false),
            Event::Warning,
        ),
        (
            Err(UpdateFailure::Verification),
            Event::Error(UpdateFailure::Verification),
        ),
    ] {
        let (backend, ledger) = fixture(snapshot(Scope::Session, Some("1"), None), result, |_| {});
        let (source, accounts, executor) = backend.into_parts();
        let (owner, mut sink, launcher) = executor.into_parts();
        sink.fail = true;
        let mut backend = ConfiguredBackend::new(
            source,
            accounts,
            PreSessionExecutor::new(owner, sink, launcher),
        );
        let (status, out, err) = run(&["task"], &mut backend);
        assert_eq!(status, 2);
        assert!(out.is_empty());
        assert_eq!(err, "error: Pre-session diagnostic delivery failed\n");
        assert_eq!(
            *ledger.borrow(),
            [
                Event::Snapshot(Scope::Session),
                Event::Account,
                Event::Prepare,
                expected
            ]
        );
        let (_, _, executor) = backend.into_parts();
        assert!(executor.into_parts().2.calls.is_empty());
    }
}

#[test]
fn unsupported_and_malformed_routes_never_access_injected_interfaces() {
    for (args, status) in [
        (vec!["help"], 0),
        (vec!["version"], 0),
        (vec!["acp"], 2),
        (vec!["serve", "--stop"], 2),
        (vec!["--update", "ls"], 2),
        (vec!["update", "--quiet"], 2),
        (vec!["--upgrade"], 2),
    ] {
        let (mut backend, ledger) = fixture(
            snapshot(Scope::Session, Some("1"), Some("1")),
            Err(UpdateFailure::Other),
            |_| panic!("no owner"),
        );
        assert_eq!(run(&args, &mut backend).0, status, "{args:?}");
        assert!(ledger.borrow().is_empty(), "{args:?}");
    }
    for update in [
        json!(null),
        json!("private-raw"),
        json!(1),
        json!([]),
        json!({}),
    ] {
        let (mut backend, ledger) = fixture(
            snapshot(Scope::Session, None, None),
            Err(UpdateFailure::Other),
            |_| panic!("no owner"),
        );
        let error = backend.execute(Request { version: 1, command: json!({"operation":"prompt","arguments":{"prompt":"task","options":{"update":update}}}) }).err().unwrap();
        assert_eq!(error.code, "configuration");
        assert!(!format!("{error:?}").contains("private-"));
        assert!(ledger.borrow().is_empty());
    }
    for args in [
        vec!["f5y", "run", "--task", "task", "--update"],
        vec!["f5y", "serve", "--update"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn shared_contexts_do_not_apply_updates_to_session_queries_start_or_connect() {
    for (args, scope) in [
        (vec!["ls"], Scope::Session),
        (vec!["session", "list"], Scope::Session),
        (vec!["session", "get", "session-A"], Scope::Session),
        (vec!["session", "start", "--prompt", "task"], Scope::Session),
        (vec!["connect"], Scope::Connect),
    ] {
        let (mut backend, ledger) = fixture(
            snapshot(scope, Some("1"), Some("1")),
            Err(UpdateFailure::Other),
            |_| panic!("no owner"),
        );
        assert_eq!(run(&args, &mut backend).0, 7);
        let expected = if scope == Scope::Session {
            vec![Event::Snapshot(scope), Event::Account, Event::Installed]
        } else {
            vec![Event::Snapshot(scope), Event::Installed]
        };
        assert_eq!(*ledger.borrow(), expected);
        let (_, context) = launched(backend);
        if let ConfigurationContext::Session { pre_session, .. } = context {
            assert!(pre_session.is_none());
        }
    }
}

#[test]
fn input_failure_and_launch_error_do_not_retry_or_reenter_preparation() {
    let (backend, ledger) = fixture(
        snapshot(Scope::Session, None, None),
        prepared(PreparationOutcome::Installed, false),
        |_| panic!("no owner"),
    );
    let (mut source, accounts, executor) = backend.into_parts();
    source.input = None;
    let mut backend = ConfiguredBackend::new(source, accounts, executor);
    assert_eq!(
        run(&["task"], &mut backend),
        (
            2,
            String::new(),
            "error: Configuration inputs unavailable\n".into()
        )
    );
    assert_eq!(*ledger.borrow(), [Event::Snapshot(Scope::Session)]);
    let (backend, ledger) = fixture(
        snapshot(Scope::Session, Some("1"), None),
        prepared(PreparationOutcome::Installed, false),
        |_| {},
    );
    let (source, accounts, executor) = backend.into_parts();
    let (owner, sink, mut launcher) = executor.into_parts();
    launcher.fail = true;
    let mut backend = ConfiguredBackend::new(
        source,
        accounts,
        PreSessionExecutor::new(owner, sink, launcher),
    );
    assert_eq!(
        run(&["task"], &mut backend),
        (2, String::new(), "error: Launch unavailable\n".into())
    );
    assert_eq!(
        *ledger.borrow(),
        [
            Event::Snapshot(Scope::Session),
            Event::Account,
            Event::Prepare,
            Event::Installed
        ]
    );
    launched(backend);
}
