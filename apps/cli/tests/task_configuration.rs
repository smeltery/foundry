#[path = "support_configuration/task_cases.rs"]
mod cases;
mod support_configuration;
use f5y::{Backend, Request, configuration::*};
use serde_json::{Value, json};
use support_configuration::*;

fn session(key: Option<&str>, endpoint: Option<&str>) -> InputSnapshot {
    InputSnapshot::Session(SessionInputs {
        environment_key: key.and_then(private),
        selected_login: private("private-login-B"),
        environment_endpoint: endpoint.and_then(private),
        updates: UpdateInputs::default(),
    })
}

#[test]
fn every_session_and_task_route_consumes_one_snapshot_and_endpoint_bound_workspace() {
    for (route, expected) in cases::routes().into_iter().chain(cases::explicit_tasks()) {
        for (key, origin, workspace) in [
            (Some("private-key-A"), Origin::EnvironmentKey, "workspace-A"),
            (None, Origin::SelectedLogin, "workspace-B"),
        ] {
            for (endpoint, suffix) in [(ENDPOINT_A, "@A"), (ENDPOINT_B, "@B")] {
                let mut backend = fixture(session(key, Some(endpoint)));
                let (code, out, err) = run(&route, &mut backend);
                assert_eq!(code, 3, "{route:?}");
                assert_eq!(out, b"consumed\n");
                assert!(err.is_empty());
                let (source, accounts, consumer) = backend.into_parts();
                assert_eq!(source.calls, [Scope::Session]);
                assert_eq!(accounts.calls, [origin]);
                assert_eq!(consumer.calls.len(), 1);
                assert_eq!(consumer.endpoints, accounts.endpoints);
                let (request, context) = &consumer.calls[0];
                assert_eq!(
                    serde_json::to_value(request).unwrap(),
                    json!({"version":1,"command":expected})
                );
                assert_private_request(request);
                let ConfigurationContext::Session {
                    identity: selected,
                    endpoint: received,
                    ..
                } = context
                else {
                    panic!("session context")
                };
                identity(selected, origin, &format!("{workspace}{suffix}"));
                let expected_endpoint =
                    (None, vec![(Origin::EnvironmentEndpoint, endpoint.into())]);
                assert_eq!(endpoint_view(received), expected_endpoint);
                assert_eq!(accounts.endpoints, [expected_endpoint]);
            }
        }
    }
}

#[test]
fn missing_empty_and_opaque_endpoints_retain_selection_without_defaults() {
    for route in [
        vec!["task"],
        vec!["run", "--task", "task"],
        vec!["ls"],
        vec!["session", "get", "id"],
    ] {
        for (value, cause) in [
            (None, Some(PendingCause::Missing)),
            (Some(""), Some(PendingCause::OwnerPolicy)),
            (Some(" opaque private-endpoint λ "), None),
        ] {
            let mut backend = fixture(session(Some("private-key-A"), value));
            assert_eq!(run(&route, &mut backend).0, 3);
            let (source, accounts, consumer) = backend.into_parts();
            assert_eq!(source.calls, [Scope::Session]);
            assert_eq!(accounts.calls, [Origin::EnvironmentKey]);
            let ConfigurationContext::Session {
                identity: selected,
                endpoint,
                ..
            } = &consumer.calls[0].1
            else {
                panic!("session context")
            };
            identity(selected, Origin::EnvironmentKey, "workspace-A");
            let candidates = value
                .into_iter()
                .map(|v| (Origin::EnvironmentEndpoint, v.into()))
                .collect();
            let expected = (cause, candidates);
            assert_eq!(endpoint_view(endpoint), expected);
            assert_eq!(accounts.endpoints, [expected]);
            assert_private_request(&consumer.calls[0].0);
        }
    }
}

#[test]
fn task_identity_failures_do_not_retry_login_or_execute_and_keep_errors_private() {
    for route in [vec!["task"], vec!["run", "--task", "task"]] {
        for fail_inputs in [false, true] {
            for json_output in [false, true] {
                let mut backend = ConfiguredBackend::new(
                    Source {
                        input: if fail_inputs {
                            None
                        } else {
                            Some(session(Some("private-bad-key"), Some(ENDPOINT_A)))
                        },
                        calls: vec![],
                    },
                    AccountMap::default(),
                    Consumer::default(),
                );
                let mut argv = route.clone();
                if json_output {
                    argv.push("--json");
                }
                let (code, out, err) = run(&argv, &mut backend);
                assert_eq!(code, 2);
                assert!(out.is_empty());
                let message = if fail_inputs {
                    "Configuration inputs unavailable"
                } else {
                    "Account resolution unavailable"
                };
                if json_output {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&err).unwrap(),
                        json!({"error":{"code":"configuration","message":message}})
                    );
                } else {
                    assert_eq!(err, format!("error: {message}\n").as_bytes());
                }
                let (source, accounts, consumer) = backend.into_parts();
                assert_eq!(source.calls, [Scope::Session]);
                assert_eq!(
                    accounts.calls,
                    if fail_inputs {
                        vec![]
                    } else {
                        vec![Origin::EnvironmentKey]
                    }
                );
                assert!(consumer.calls.is_empty());
                if !fail_inputs {
                    assert_eq!(
                        accounts.endpoints,
                        [(None, vec![(Origin::EnvironmentEndpoint, ENDPOINT_A.into())])]
                    );
                }
            }
        }
    }
}

#[test]
fn empty_key_does_not_fall_back_to_login_for_tasks() {
    for route in [vec!["task"], vec!["run", "--task", "task"]] {
        let mut backend = fixture(session(Some(""), Some(ENDPOINT_A)));
        assert_eq!(run(&route, &mut backend).0, 3);
        let (source, accounts, consumer) = backend.into_parts();
        assert_eq!(source.calls, [Scope::Session]);
        assert!(accounts.calls.is_empty());
        let ConfigurationContext::Session {
            identity: Selection::Pending { cause, candidates },
            endpoint,
            ..
        } = &consumer.calls[0].1
        else {
            panic!("pending identity")
        };
        assert_eq!(*cause, PendingCause::OwnerPolicy);
        assert_eq!(
            candidates
                .iter()
                .map(|c| (c.origin, c.value.expose()))
                .collect::<Vec<_>>(),
            [
                (Origin::EnvironmentKey, ""),
                (Origin::SelectedLogin, "private-login-B")
            ]
        );
        assert_eq!(
            endpoint_view(endpoint),
            (None, vec![(Origin::EnvironmentEndpoint, ENDPOINT_A.into())])
        );
    }
}

#[test]
fn task_results_and_executor_errors_use_normal_channels_without_private_context() {
    for route in [vec!["task"], vec!["run", "--task", "task"]] {
        for format in [None, Some("--json"), Some("--output-format=json")] {
            for fail in [false, true] {
                let mut backend = ConfiguredBackend::new(
                    Source {
                        input: Some(session(Some("private-key-A"), Some(ENDPOINT_A))),
                        calls: vec![],
                    },
                    AccountMap::default(),
                    Consumer {
                        fail,
                        ..Consumer::default()
                    },
                );
                let mut argv = route.clone();
                argv.extend(format);
                let (code, out, err) = run(&argv, &mut backend);
                assert_eq!(code, if fail { 2 } else { 3 });
                if fail {
                    assert!(out.is_empty());
                    if format.is_some() {
                        assert_eq!(
                            serde_json::from_slice::<Value>(&err).unwrap(),
                            json!({"error":{"code":"fixture","message":"Executor unavailable"}})
                        );
                    } else {
                        assert_eq!(err, b"error: Executor unavailable\n");
                    }
                } else {
                    assert!(err.is_empty());
                    assert_eq!(
                        out,
                        if format.is_some() {
                            b"{\"consumed\":true}\n".as_slice()
                        } else {
                            b"consumed\n"
                        }
                    );
                }
                let (source, accounts, consumer) = backend.into_parts();
                assert_eq!(source.calls, [Scope::Session]);
                assert_eq!(accounts.calls, [Origin::EnvironmentKey]);
                assert_eq!(consumer.calls.len(), 1);
                assert_eq!(consumer.endpoints, accounts.endpoints);
                assert_private_request(&consumer.calls[0].0);
            }
        }
    }
}

#[test]
fn malformed_task_envelopes_and_unsupported_acp_never_read_inputs() {
    let mut commands = vec![
        Value::Null,
        json!([]),
        json!({}),
        json!({"operation":7,"arguments":{}}),
        json!({"operation":"session","arguments":{"operation":"unavailable","arguments":{}}}),
    ];
    for args in [None, Some(Value::Null), Some(json!([]))] {
        for operation in ["prompt", "run", "session"] {
            let mut command = json!({"operation":operation});
            if let Some(args) = &args {
                command["arguments"] = args.clone();
            }
            commands.push(command);
        }
        let mut arguments = json!({"prompt":"task"});
        if let Some(args) = &args {
            arguments["options"] = args.clone();
        }
        commands.push(json!({"operation":"prompt","arguments":arguments}));
    }
    for command in commands {
        let mut backend = fixture(session(Some("private-key-A"), Some(ENDPOINT_A)));
        assert!(
            backend
                .execute(Request {
                    version: 1,
                    command
                })
                .is_err()
        );
        let (source, accounts, consumer) = backend.into_parts();
        assert!(source.calls.is_empty());
        assert!(accounts.calls.is_empty());
        assert!(consumer.calls.is_empty());
    }
    for argv in [
        vec!["acp"],
        vec!["acp", "--effort", "high"],
        vec!["--cloud", "ls"],
        vec!["--effort", "high", "status"],
    ] {
        let mut backend = fixture(session(Some("private-key-A"), Some(ENDPOINT_A)));
        assert_eq!(run(&argv, &mut backend).0, 2);
        let (source, accounts, consumer) = backend.into_parts();
        assert!(source.calls.is_empty());
        assert!(accounts.calls.is_empty());
        assert!(consumer.calls.is_empty());
    }
}

#[test]
fn connect_endpoint_selection_reaches_accounts_and_executor_without_coalescing() {
    for command in ["connect", "device"] {
        for (explicit, environment, saved, cause) in [
            (None, None, None, Some(PendingCause::Missing)),
            (Some(""), None, None, Some(PendingCause::OwnerPolicy)),
            (Some("private-cli"), None, None, None),
            (None, Some("private-env"), None, None),
            (None, None, Some("private-saved"), None),
            (
                Some("private-same"),
                Some("private-same"),
                Some("private-same"),
                Some(PendingCause::Competing),
            ),
            (
                Some("private-cli"),
                Some("private-env"),
                Some("private-saved"),
                Some(PendingCause::Competing),
            ),
        ] {
            let mut backend = fixture(InputSnapshot::Connect(ConnectInputs {
                environment_key: private("private-key-A"),
                environment_endpoint: environment.and_then(private),
                saved_endpoint: saved.and_then(private),
                ..ConnectInputs::default()
            }));
            let mut argv = vec![command];
            if let Some(explicit) = explicit {
                argv.extend(["--api-url", explicit]);
            }
            assert_eq!(run(&argv, &mut backend).0, 3);
            let (source, accounts, consumer) = backend.into_parts();
            assert_eq!(source.calls, [Scope::Connect]);
            assert_eq!(accounts.calls, [Origin::EnvironmentKey]);
            let candidates = [
                (Origin::ExplicitEndpoint, explicit),
                (Origin::EnvironmentEndpoint, environment),
                (Origin::SavedEndpoint, saved),
            ]
            .into_iter()
            .filter_map(|(o, v)| v.map(|v| (o, v.into())))
            .collect();
            let expected = (cause, candidates);
            assert_eq!(accounts.endpoints, std::slice::from_ref(&expected));
            assert_eq!(consumer.endpoints, accounts.endpoints);
            let ConfigurationContext::Connect {
                endpoint, proxy, ..
            } = &consumer.calls[0].1
            else {
                panic!("connect context")
            };
            assert_eq!(endpoint_view(endpoint), expected);
            assert!(proxy.excludes_https_proxy());
            assert_private_request(&consumer.calls[0].0);
        }
    }
}

#[test]
fn unsupported_request_version_rejects_before_configuration() {
    let mut backend = fixture(session(Some("private-key-A"), Some(ENDPOINT_A)));
    let (_, command) = cases::routes().remove(0);
    let error = backend
        .execute(Request {
            version: 2,
            command,
        })
        .err()
        .unwrap();
    assert_eq!(error.code, "configuration");
    let (source, accounts, consumer) = backend.into_parts();
    assert!(source.calls.is_empty());
    assert!(accounts.calls.is_empty());
    assert!(consumer.calls.is_empty());
}
