mod support_update_notice;

use f5y::configuration::*;
use serde_json::json;
use support_update_notice::*;

#[test]
fn documented_positive_sources_prepare_once_before_launch() {
    for (args, route, scope, explicit, auto) in [
        (
            vec!["task", "--update"],
            UpdateRoute::Prompt,
            Scope::Session,
            true,
            None,
        ),
        (
            vec!["task"],
            UpdateRoute::Prompt,
            Scope::Session,
            false,
            Some("1"),
        ),
        (
            vec!["task", "--update"],
            UpdateRoute::Prompt,
            Scope::Session,
            true,
            Some("1"),
        ),
        (
            vec!["run", "--task", "task"],
            UpdateRoute::Run,
            Scope::Session,
            false,
            Some("1"),
        ),
        (
            vec!["serve"],
            UpdateRoute::Serve,
            Scope::Serve,
            false,
            Some("1"),
        ),
    ] {
        let (mut backend, ledger) = fixture(
            snapshot(scope, auto, None),
            prepared(PreparationOutcome::Installed, false),
            move |plan| {
                assert_eq!(plan.route, route);
                assert_eq!(plan.explicit_update, explicit);
                observation(&plan.auto_update, auto);
                observation(&plan.no_update_notice, None);
                assert!(plan.update_requested());
                assert!(!plan.daily_notice_suppressed());
            },
        );
        assert_eq!(
            run(&args, &mut backend),
            (7, "installed task\n".into(), String::new())
        );
        let mut expected = vec![Event::Snapshot(scope)];
        if scope == Scope::Session {
            expected.push(Event::Account);
        }
        expected.extend([Event::Prepare, Event::Installed]);
        assert_eq!(*ledger.borrow(), expected);
        let (_, context) = launched(backend);
        if scope == Scope::Session {
            assert_session(&context);
        } else {
            let ConfigurationContext::Serve(serve) = context else {
                panic!("serve context")
            };
            let Selection::Selected(ServeIdentity::TokenBound(candidate)) = &serve.identity else {
                panic!("token binding")
            };
            assert_eq!(candidate.value.expose(), "private-token");
            assert_eq!(
                serve.persistence,
                ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
            );
            assert_eq!(serve.pre_session.route, UpdateRoute::Serve);
        }
    }
}

#[test]
fn updated_handoff_drives_distinct_launcher_and_preserves_request_context_and_response() {
    for json_output in [false, true] {
        let result = prepared(
            PreparationOutcome::Updated(UpdatedLaunch::new(PrivateValue::new(
                "private-verified-handoff",
            ))),
            false,
        );
        let (mut backend, ledger) =
            fixture(snapshot(Scope::Session, Some("1"), None), result, |plan| {
                assert!(plan.update_requested())
            });
        let mut args = vec![
            "task",
            "--update",
            "--model",
            "model-A",
            "--effort",
            "high",
            "--on",
            "worker-A",
            "--tool-grant",
            "read",
        ];
        if json_output {
            args.push("--json");
        }
        let (status, out, err) = run(&args, &mut backend);
        assert_eq!(status, 9);
        assert!(err.is_empty());
        if json_output {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&out).unwrap(),
                json!({"result":"updated"})
            );
        } else {
            assert_eq!(out, "updated task\n");
        }
        assert_eq!(
            *ledger.borrow(),
            [
                Event::Snapshot(Scope::Session),
                Event::Account,
                Event::Prepare,
                Event::Updated("private-verified-handoff".into())
            ]
        );
        let (request, context) = launched(backend);
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"version":1,"command":{"operation":"prompt","arguments":{
                "prompt":"task","options":{"cloud":false,"on":"worker-A","dir":null,"yes":false,"detach":false,
                "repo":null,"no_repo":false,"model":"model-A","effort":"high","resume":null,"update":true,"upgrade":false,
                "tool_grants":["read"]}
            }}})
        );
        assert_session(&context);
        let ConfigurationContext::Session {
            pre_session: Some(plan),
            ..
        } = context
        else {
            panic!("retained plan")
        };
        assert!(plan.explicit_update);
        observation(&plan.auto_update, Some("1"));
    }
}

#[test]
fn notice_suppression_never_suppresses_unreachable_warning_or_requested_update() {
    for suppression in [None, Some("1")] {
        for available_notice in [false, true] {
            for unreachable in [false, true] {
                let outcome = if unreachable {
                    PreparationOutcome::ReleaseServerUnavailable
                } else {
                    PreparationOutcome::Installed
                };
                let (mut backend, ledger) = fixture(
                    snapshot(Scope::Session, Some("1"), suppression),
                    prepared(outcome, available_notice),
                    move |plan| {
                        assert!(plan.update_requested());
                        assert_eq!(plan.daily_notice_suppressed(), suppression == Some("1"));
                    },
                );
                assert_eq!(
                    run(&["run", "--task", "task"], &mut backend),
                    (7, "installed task\n".into(), String::new())
                );
                let mut expected = vec![
                    Event::Snapshot(Scope::Session),
                    Event::Account,
                    Event::Prepare,
                ];
                if available_notice && suppression.is_none() {
                    expected.push(Event::Daily);
                }
                if unreachable {
                    expected.push(Event::Warning);
                }
                expected.push(Event::Installed);
                assert_eq!(*ledger.borrow(), expected);
                launched(backend);
            }
        }
    }
}

#[test]
fn raw_settings_are_preserved_for_owner_policy_not_coerced() {
    for raw in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("01"),
        Some(" 1 "),
        Some("１"),
        Some("private-unknown λ"),
    ] {
        for explicit in [false, true] {
            let (mut backend, ledger) = fixture(
                snapshot(Scope::Session, raw, raw),
                Err(UpdateFailure::PolicyUnresolved),
                move |plan| {
                    observation(&plan.auto_update, raw);
                    observation(&plan.no_update_notice, raw);
                    assert_eq!(plan.explicit_update, explicit);
                    assert_eq!(plan.update_requested(), explicit);
                    assert!(!plan.daily_notice_suppressed());
                },
            );
            let args = if explicit {
                vec!["task", "--update"]
            } else {
                vec!["task"]
            };
            let (status, out, err) = run(&args, &mut backend);
            assert_eq!(status, 2);
            assert!(out.is_empty());
            assert!(!err.contains("private-unknown"));
            assert_eq!(
                *ledger.borrow(),
                [
                    Event::Snapshot(Scope::Session),
                    Event::Account,
                    Event::Prepare,
                    Event::Error(UpdateFailure::PolicyUnresolved)
                ]
            );
            let (_, _, executor) = backend.into_parts();
            assert!(executor.into_parts().2.calls.is_empty());
        }
    }
}

#[test]
fn owner_can_resolve_unknown_notice_without_discarding_raw_value() {
    for suppress in [false, true] {
        let preparation = Preparation {
            outcome: PreparationOutcome::Installed,
            daily_notice: true,
            notice: if suppress {
                NoticeDecision::Suppress
            } else {
                NoticeDecision::Allow
            },
        };
        let (mut backend, ledger) = fixture(
            snapshot(Scope::Session, None, Some("private-policy")),
            Ok(preparation),
            |plan| {
                observation(&plan.no_update_notice, Some("private-policy"));
                assert!(!plan.daily_notice_suppressed());
            },
        );
        assert_eq!(run(&["task"], &mut backend).0, 7);
        let mut expected = vec![
            Event::Snapshot(Scope::Session),
            Event::Account,
            Event::Prepare,
        ];
        if !suppress {
            expected.push(Event::Daily);
        }
        expected.push(Event::Installed);
        assert_eq!(*ledger.borrow(), expected);
        launched(backend);
    }
}

#[test]
fn updated_run_and_serve_preserve_route_specific_intents() {
    for (args, scope, expected) in [
        (
            vec![
                "run", "--task", "task", "--model", "model-A", "--effort", "high",
            ],
            Scope::Session,
            json!({"operation":"run","arguments":{"task":"task","repo":null,"git_ref":null,
                "model":"model-A","effort":"high","detach":false}}),
        ),
        (
            vec![
                "serve",
                "--service",
                "--yes",
                "--sandbox",
                "--dir",
                "relative/work",
            ],
            Scope::Serve,
            json!({"operation":"serve","arguments":{"name":null,"dir":"relative/work",
                "service":true,"once":false,"yes":true,"sandbox":true,"stop":false}}),
        ),
    ] {
        let result = prepared(
            PreparationOutcome::Updated(UpdatedLaunch::new(PrivateValue::new(
                "private-verified-handoff",
            ))),
            false,
        );
        let (mut backend, ledger) = fixture(snapshot(scope, Some("1"), None), result, |plan| {
            assert!(plan.update_requested())
        });
        assert_eq!(
            run(&args, &mut backend),
            (9, "updated task\n".into(), String::new())
        );
        let mut events = vec![Event::Snapshot(scope)];
        if scope == Scope::Session {
            events.push(Event::Account);
        }
        events.extend([
            Event::Prepare,
            Event::Updated("private-verified-handoff".into()),
        ]);
        assert_eq!(*ledger.borrow(), events);
        let (request, context) = launched(backend);
        assert_eq!(request.version, 1);
        assert_eq!(request.command, expected);
        if scope == Scope::Session {
            assert_session(&context);
        } else {
            let ConfigurationContext::Serve(serve) = context else {
                panic!("serve context")
            };
            assert_eq!(
                serve.execution,
                ServeExecution::ForegroundJoinThenBackground
            );
            assert_eq!(
                serve.persistence,
                ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
            );
            assert!(serve.yes && serve.sandbox);
            let ProfileValue::Selected(directory) = &serve.directory else {
                panic!("explicit directory")
            };
            assert_eq!(directory.origin, ProfileOrigin::Explicit);
            assert_eq!(directory.value, "relative/work");
            let Selection::Selected(ServeIdentity::TokenBound(token)) = &serve.identity else {
                panic!("opaque token binding")
            };
            assert_eq!(token.value.expose(), "private-token");
            assert_eq!(serve.pre_session.route, UpdateRoute::Serve);
            observation(&serve.pre_session.auto_update, Some("1"));
        }
    }
}
