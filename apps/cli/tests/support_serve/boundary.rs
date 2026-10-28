use super::*;
use f5y::Backend;

#[test]
fn stop_and_malformed_requests_bypass_all_interfaces() {
    let mut commands = vec![
        json!({"stop":true,"token":"private-token"}),
        json!({"stop":true,"name":[]}),
    ];
    for field in [
        "token", "name", "dir", "once", "service", "yes", "sandbox", "stop",
    ] {
        commands.push(json!({field:["private-malformed"]}));
    }
    commands.extend([json!(null), json!([]), json!("private-envelope")]);
    for arguments in commands {
        let stopping = arguments["stop"] == true;
        let mut backend = fixture(inputs(), |_| panic!("executor must not run"));
        let error = backend.execute(request(arguments)).err().unwrap();
        assert_eq!(
            error.code,
            if stopping {
                "unsupported"
            } else {
                "configuration"
            }
        );
        assert!(!serde_json::to_string(&error).unwrap().contains("private-"));
        assert!(!format!("{error:?}").contains("private-"));
        let (source, _, consumer) = backend.into_parts();
        assert!(source.calls.is_empty());
        assert!(consumer.requests.is_empty());
    }
}

#[test]
fn snapshot_failures_and_executor_results_use_normal_channels() {
    for snapshot in [None, Some(InputSnapshot::Session(SessionInputs::default()))] {
        let backend = fixture(inputs(), |_| panic!("executor must not run"));
        let (mut source, accounts, consumer) = backend.into_parts();
        source.snapshot = snapshot;
        let mut backend = ConfiguredBackend::new(source, accounts, consumer);
        let (code, out, err) = run(&["serve", "--token", "private-token"], &mut backend);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(
            err.contains("Configuration inputs unavailable")
                || err.contains("Invalid configured backend request")
        );
        assert!(!err.contains("private-"));
        let (source, _, consumer) = backend.into_parts();
        assert_eq!(source.calls, [Scope::Serve]);
        assert!(consumer.requests.is_empty());
    }
    for fail in [false, true] {
        let backend = fixture(inputs(), |context| {
            pending_identity(context, PendingCause::Missing, &[])
        });
        let (source, accounts, mut consumer) = backend.into_parts();
        consumer.fail = fail;
        let mut backend = ConfiguredBackend::new(source, accounts, consumer);
        let (code, out, err) = run(&["--json", "serve"], &mut backend);
        assert_eq!(code, if fail { 2 } else { 3 });
        if fail {
            assert!(out.is_empty());
            let output: Value = serde_json::from_str(&err).unwrap();
            assert_eq!(
                output,
                json!({"error":{"code":"fixture","message":"Worker unavailable"}})
            );
        } else {
            assert!(err.is_empty());
            let output: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(output, json!({"consumed":true}));
        }
        consumed(backend);
    }
}

#[test]
fn persistence_obligations_are_consumed_for_join_reconnect_once_and_service() {
    for saved in [false, true] {
        for mode in ["normal", "once", "service"] {
            let mut facts = inputs();
            if saved {
                facts.saved_worker_identity = private("private-worker");
            } else {
                facts.environment_token = private("private-token");
            }
            let mut backend = fixture(facts, move |context| {
                assert_eq!(
                    context.persistence,
                    if mode == "once" {
                        ServePersistence::NoSave
                    } else if saved {
                        ServePersistence::ReuseSavedWorkerIdentity
                    } else {
                        ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
                    }
                );
                assert_eq!(
                    context.execution,
                    if mode == "service" {
                        ServeExecution::ForegroundJoinThenBackground
                    } else {
                        ServeExecution::Foreground
                    }
                );
                if saved {
                    assert!(matches!(
                        context.identity,
                        Selection::Selected(ServeIdentity::SavedWorker(_))
                    ));
                } else {
                    assert!(matches!(
                        context.identity,
                        Selection::Selected(ServeIdentity::TokenBound(_))
                    ));
                }
            });
            let args = match mode {
                "once" => vec!["serve", "--once"],
                "service" => vec!["serve", "--service"],
                _ => vec!["serve"],
            };
            assert_eq!(run(&args, &mut backend).0, 3);
            consumed(backend);
        }
    }
}
