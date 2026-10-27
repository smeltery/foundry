mod support_configuration;
use f5y::{Backend, Request, configuration::*};
use serde_json::{Value, json};
use support_configuration::*;

#[test]
fn session_selection_reaches_consumer_for_each_supported_route() {
    for route in [
        vec!["ls"],
        vec!["watch", "id"],
        vec!["send", "id", "message"],
        vec!["cancel", "id"],
        vec!["session", "get", "id"],
        vec!["session", "list"],
        vec!["session", "start", "--prompt", "task"],
        vec!["session", "watch", "id"],
        vec!["session", "send", "id", "--message", "message"],
        vec!["session", "cancel", "id"],
        vec!["session", "attachments", "id"],
        vec!["session", "download", "id"],
        vec!["session", "share-link", "id"],
    ] {
        for (key, login, origin, workspace) in [
            (
                Some("private-key-A"),
                Some("private-login-B"),
                Origin::EnvironmentKey,
                "workspace-A",
            ),
            (
                None,
                Some("private-login-B"),
                Origin::SelectedLogin,
                "workspace-B",
            ),
            (
                Some("private-key-A"),
                None,
                Origin::EnvironmentKey,
                "workspace-A",
            ),
        ] {
            let mut backend = fixture(InputSnapshot::Session(SessionInputs {
                environment_key: key.and_then(private),
                selected_login: login.and_then(private),
            }));
            let (code, out, err) = run(&route, &mut backend);
            assert_eq!(code, 3);
            assert_eq!(out, b"consumed\n");
            assert!(err.is_empty());
            let (source, accounts, consumer) = backend.into_parts();
            assert_eq!(source.calls, [Scope::Session]);
            assert_eq!(accounts.calls, [origin]);
            assert_eq!(consumer.calls.len(), 1);
            let (request, context) = &consumer.calls[0];
            let ConfigurationContext::Session { identity: selected } = context else {
                panic!("session context")
            };
            identity(selected, origin, workspace);
            let encoded = serde_json::to_string(request).unwrap();
            let debug = format!("{request:?}");
            assert!(!encoded.contains("private-"));
            assert!(!debug.contains("private-"));
        }
    }
}

#[test]
fn connect_single_keys_and_proxy_exclusion_reach_consumer() {
    for command in ["connect", "device"] {
        for explicit_key in [false, true] {
            for explicit_proxy in [false, true] {
                for ambient in [None, Some("private-ambient-A"), Some("private-ambient-B")] {
                    let mut backend = fixture(InputSnapshot::Connect(ConnectInputs {
                        environment_key: if explicit_key {
                            None
                        } else {
                            private("private-key-A")
                        },
                        https_proxy: ambient.and_then(private),
                        ..ConnectInputs::default()
                    }));
                    let mut args = vec![command, "--name", "worker", "--json"];
                    if explicit_key {
                        args.extend(["--api-key", "private-key-A"]);
                    }
                    if explicit_proxy {
                        args.extend(["--proxy", "http://private-user:private-pass@proxy.invalid"]);
                    }
                    let (code, out, err) = run(&args, &mut backend);
                    assert_eq!(code, 3);
                    assert_eq!(
                        serde_json::from_slice::<Value>(&out).unwrap(),
                        json!({"consumed":true})
                    );
                    assert!(err.is_empty());
                    let (source, accounts, consumer) = backend.into_parts();
                    assert_eq!(source.calls, [Scope::Connect]);
                    assert_eq!(accounts.calls.len(), 1);
                    let (request, context) = &consumer.calls[0];
                    assert_eq!(request.command["arguments"]["name"], "worker");
                    for field in ["api_key", "token", "api_url", "proxy"] {
                        assert!(request.command["arguments"].get(field).is_none());
                    }
                    assert!(!format!("{request:?}").contains("private-"));
                    assert!(!serde_json::to_string(request).unwrap().contains("private-"));
                    let ConfigurationContext::Connect {
                        identity: selected,
                        proxy,
                        ..
                    } = context
                    else {
                        panic!("connect context")
                    };
                    identity(
                        selected,
                        if explicit_key {
                            Origin::ExplicitKey
                        } else {
                            Origin::EnvironmentKey
                        },
                        "workspace-A",
                    );
                    assert!(proxy.excludes_https_proxy());
                    match &proxy.selection {
                        Selection::Selected(candidate) => {
                            assert!(explicit_proxy);
                            assert_eq!(candidate.origin, Origin::ExplicitProxy);
                            assert_eq!(
                                candidate.value.expose(),
                                "http://private-user:private-pass@proxy.invalid"
                            );
                        }
                        Selection::Pending { cause, candidates } => {
                            assert!(!explicit_proxy);
                            assert_eq!(*cause, PendingCause::Missing);
                            assert!(candidates.is_empty());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn unresolved_connect_candidates_are_consumed_without_precedence_or_coalescing() {
    for environment_key in ["private-key-A", "private-key-B"] {
        let mut backend = fixture(InputSnapshot::Connect(ConnectInputs {
            environment_key: private(environment_key),
            saved_identity: private("private-saved"),
            environment_endpoint: private("private-endpoint-env"),
            saved_endpoint: private("private-endpoint-saved"),
            saved_proxy: private("private-proxy-saved"),
            ..ConnectInputs::default()
        }));
        let (code, out, err) = run(
            &[
                "connect",
                "--api-key",
                "private-key-A",
                "--token",
                "private-token",
                "--api-url",
                "private-endpoint-cli",
                "--proxy",
                "private-proxy-cli",
            ],
            &mut backend,
        );
        assert_eq!(code, 3); // The consumer records pending policy, not successful enrollment.
        assert_eq!(out, b"consumed\n");
        assert!(err.is_empty());
        let (_, accounts, consumer) = backend.into_parts();
        assert!(accounts.calls.is_empty());
        let (request, context) = &consumer.calls[0];
        assert!(!serde_json::to_string(request).unwrap().contains("private-"));
        let ConfigurationContext::Connect {
            identity,
            endpoint,
            proxy,
        } = context
        else {
            panic!("connect context")
        };
        let Selection::Pending { cause, candidates } = identity else {
            panic!("pending identities")
        };
        assert_eq!(*cause, PendingCause::Competing);
        assert_eq!(
            candidates
                .iter()
                .map(|c| (c.origin, c.value.expose()))
                .collect::<Vec<_>>(),
            vec![
                (Origin::ExplicitKey, "private-key-A"),
                (Origin::EnvironmentKey, environment_key),
                (Origin::EnrollmentToken, "private-token"),
                (Origin::SavedIdentity, "private-saved")
            ]
        );
        for (selection, expected) in [
            (
                endpoint,
                vec![
                    "private-endpoint-cli",
                    "private-endpoint-env",
                    "private-endpoint-saved",
                ],
            ),
            (
                &proxy.selection,
                vec!["private-proxy-cli", "private-proxy-saved"],
            ),
        ] {
            let Selection::Pending { cause, candidates } = selection else {
                panic!("pending values")
            };
            assert_eq!(*cause, PendingCause::Competing);
            assert_eq!(
                candidates
                    .iter()
                    .map(|c| c.value.expose())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}

#[test]
fn bypass_failures_and_unsupported_scope_do_not_consume_configuration() {
    for (args, expected) in [
        (vec!["help", "session"], 0),
        (vec!["version"], 0),
        (vec!["--cloud", "ls"], 2),
        (vec!["task"], 2),
        (vec!["run", "--task", "task"], 2),
        (vec!["acp"], 2),
    ] {
        let mut backend = fixture(InputSnapshot::Session(SessionInputs::default()));
        assert_eq!(run(&args, &mut backend).0, expected);
        let (source, accounts, consumer) = backend.into_parts();
        assert!(source.calls.is_empty());
        assert!(accounts.calls.is_empty());
        assert!(consumer.calls.is_empty());
    }
}

#[test]
fn injected_failures_are_safe_and_never_fallback_or_invoke_executor() {
    for fail_inputs in [false, true] {
        for json in [false, true] {
            let input = InputSnapshot::Session(SessionInputs {
                environment_key: private("private-key-A"),
                selected_login: private("private-login-B"),
            });
            let mut backend = ConfiguredBackend::new(
                Source {
                    input: if fail_inputs { None } else { Some(input) },
                    calls: vec![],
                },
                AccountMap {
                    fail: true,
                    ..AccountMap::default()
                },
                Consumer::default(),
            );
            let args = if json {
                vec!["ls", "--json"]
            } else {
                vec!["ls"]
            };
            let (code, out, err) = run(&args, &mut backend);
            assert_eq!(code, 2);
            assert!(out.is_empty());
            let message = if fail_inputs {
                "Configuration inputs unavailable"
            } else {
                "Account resolution unavailable"
            };
            if json {
                let value: Value = serde_json::from_slice(&err).unwrap();
                assert_eq!(value["error"]["code"], "configuration");
                assert_eq!(value["error"]["message"], message);
            } else {
                assert_eq!(
                    String::from_utf8(err).unwrap(),
                    format!("error: {message}\n")
                );
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
        }
    }
}

#[test]
fn empty_and_absent_identity_remain_unresolved_owner_inputs() {
    for (key, login) in [
        (None, None),
        (Some(""), None),
        (Some(""), Some("private-login-B")),
    ] {
        let mut backend = fixture(InputSnapshot::Session(SessionInputs {
            environment_key: key.and_then(private),
            selected_login: login.and_then(private),
        }));
        assert_eq!(run(&["ls"], &mut backend).0, 3);
        let (_, accounts, consumer) = backend.into_parts();
        assert!(accounts.calls.is_empty());
        let ConfigurationContext::Session {
            identity: Selection::Pending { cause, candidates },
        } = &consumer.calls[0].1
        else {
            panic!("pending identity")
        };
        assert_eq!(
            *cause,
            if key.is_some() {
                PendingCause::OwnerPolicy
            } else {
                PendingCause::Missing
            }
        );
        assert_eq!(
            candidates.len(),
            usize::from(key.is_some()) + usize::from(login.is_some())
        );
        if login.is_some() {
            assert_eq!(candidates[1].origin, Origin::SelectedLogin);
            assert_eq!(candidates[1].value.expose(), "private-login-B");
        }
    }
}

#[test]
fn malformed_private_request_and_wrong_snapshot_scope_fail_safely() {
    let mut backend = fixture(InputSnapshot::Session(SessionInputs::default()));
    let error = backend
        .execute(Request {
            version: 1,
            command: json!({"operation":"connect",
        "arguments":{"api_key":{"private":"sentinel"}}}),
        })
        .err()
        .unwrap();
    assert_eq!(error.message, "Invalid configured backend request");
    assert_eq!(run(&["connect"], &mut backend).0, 2);
    let (source, accounts, consumer) = backend.into_parts();
    assert_eq!(source.calls, [Scope::Connect]);
    assert!(accounts.calls.is_empty());
    assert!(consumer.calls.is_empty());
}

#[test]
fn malformed_envelopes_never_read_inputs_accounts_or_execute() {
    for operation in ["ls", "watch", "send", "cancel", "connect", "session"] {
        for args in [None, Some(Value::Null), Some(json!([]))] {
            for nested in [false, true] {
                let mut command = json!({"operation": operation});
                if let Some(args) = &args {
                    command["arguments"] = args.clone();
                }
                if nested {
                    command = json!({"operation":"session", "arguments":command});
                }
                let mut backend = fixture(InputSnapshot::Session(SessionInputs::default()));
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
        }
    }
}
