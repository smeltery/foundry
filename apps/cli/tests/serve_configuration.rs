mod support_serve;

use f5y::{Backend, configuration::*};
use serde_json::json;
use support_serve::*;

#[test]
fn private_tokens_are_consumed_without_decoding_or_forwarding() {
    for explicit in [false, true] {
        let mut facts = inputs();
        if !explicit {
            facts.environment_token = private("private-opaque:not-a-machine");
        }
        let mut backend = fixture(facts, move |context| {
            let Selection::Selected(ServeIdentity::TokenBound(candidate)) = &context.identity
            else {
                panic!("expected opaque token binding")
            };
            assert_eq!(
                candidate.origin,
                if explicit {
                    Origin::ExplicitToken
                } else {
                    Origin::EnvironmentToken
                }
            );
            assert_eq!(candidate.value.expose(), "private-opaque:not-a-machine");
            assert!(matches!(context.machine, ServeMachine::TokenBound));
            selected(&context.directory, ProfileOrigin::Cwd, "/injected/work");
            assert_eq!(
                context.persistence,
                ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
            );
            assert_eq!(context.execution, ServeExecution::Foreground);
            assert!(!context.yes && !context.sandbox);
        });
        let args = if explicit {
            vec!["serve", "--token", "private-opaque:not-a-machine"]
        } else {
            vec!["serve"]
        };
        assert_eq!(
            run(&args, &mut backend),
            (3, "consumed\n".into(), String::new())
        );
        assert_eq!(
            consumed(backend).command,
            json!({"operation":"serve","arguments":{
                "name":null,"dir":null,"once":false,"service":false,"sandbox":false,"yes":false,"stop":false
            }})
        );
    }
}

#[test]
fn signed_in_name_directory_and_lifetime_intents_reach_executor() {
    for (args, name, directory, once, service) in [
        (
            vec!["serve"],
            "injected-host",
            "/injected/work",
            false,
            false,
        ),
        (
            vec![
                "serve",
                "chosen",
                "--dir",
                "relative/../work",
                "--once",
                "--yes",
                "--sandbox",
            ],
            "chosen",
            "relative/../work",
            true,
            false,
        ),
        (
            vec!["serve", "chosen", "--service", "--yes", "--sandbox"],
            "chosen",
            "/injected/work",
            false,
            true,
        ),
    ] {
        let mut facts = inputs();
        facts.selected_login = private("private-login");
        let mut backend = fixture(facts, move |context| {
            let Selection::Selected(ServeIdentity::SelectedLogin(candidate)) = &context.identity
            else {
                panic!("expected supplied login")
            };
            assert_eq!(candidate.origin, Origin::SelectedLogin);
            assert_eq!(candidate.value.expose(), "private-login");
            let ServeMachine::Named(value) = &context.machine else {
                panic!("expected named intent")
            };
            selected(
                value,
                if name == "chosen" {
                    ProfileOrigin::Explicit
                } else {
                    ProfileOrigin::Hostname
                },
                name,
            );
            selected(
                &context.directory,
                if once {
                    ProfileOrigin::Explicit
                } else {
                    ProfileOrigin::Cwd
                },
                directory,
            );
            assert_eq!(
                context.persistence,
                if once {
                    ServePersistence::NoSave
                } else {
                    ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
                }
            );
            assert_eq!(
                context.execution,
                if service {
                    ServeExecution::ForegroundJoinThenBackground
                } else {
                    ServeExecution::Foreground
                }
            );
            assert_eq!(
                (context.yes, context.sandbox),
                (once || service, once || service)
            );
        });
        assert_eq!(run(&args, &mut backend).0, 3);
        let request = consumed(backend);
        let arguments = &request.command["arguments"];
        assert_eq!(arguments["once"], once);
        assert_eq!(arguments["service"], service);
        assert_eq!(arguments["yes"], once || service);
        assert_eq!(arguments["sandbox"], once || service);
        if once {
            assert_eq!(arguments["dir"], "relative/../work");
        }
    }
}

#[test]
fn saved_worker_reconnect_does_not_invent_machine_or_hostname() {
    for named in [false, true] {
        let mut facts = inputs();
        facts.saved_worker_identity = private("private-worker");
        let mut backend = fixture(facts, move |context| {
            let Selection::Selected(ServeIdentity::SavedWorker(candidate)) = &context.identity
            else {
                panic!("expected supplied saved worker")
            };
            assert_eq!(candidate.origin, Origin::SavedWorkerIdentity);
            assert_eq!(candidate.value.expose(), "private-worker");
            if named {
                let ServeMachine::Pending { name } = &context.machine else {
                    panic!("unverified association")
                };
                pending(
                    name,
                    PendingCause::OwnerPolicy,
                    &[(ProfileOrigin::Explicit, "chosen")],
                );
            } else {
                assert!(matches!(context.machine, ServeMachine::SavedWorker));
            }
        });
        assert_eq!(
            run(
                &if named {
                    vec!["serve", "chosen"]
                } else {
                    vec!["serve"]
                },
                &mut backend
            )
            .0,
            3
        );
        consumed(backend);
    }
}

#[test]
fn competing_credentials_preserve_every_origin_even_equal_bytes() {
    for mask in 1u8..16 {
        if mask.count_ones() < 2 {
            continue;
        }
        let mut facts = inputs();
        if mask & 2 != 0 {
            facts.environment_token = private("private-same");
        }
        if mask & 4 != 0 {
            facts.saved_worker_identity = private("private-same");
        }
        if mask & 8 != 0 {
            facts.selected_login = private("private-same");
        }
        let mut backend = fixture(facts, move |context| {
            let origins = [
                (1, Origin::ExplicitToken),
                (2, Origin::EnvironmentToken),
                (4, Origin::SavedWorkerIdentity),
                (8, Origin::SelectedLogin),
            ];
            let expected: Vec<_> = origins
                .into_iter()
                .filter(|(bit, _)| mask & bit != 0)
                .map(|(_, origin)| (origin, "private-same"))
                .collect();
            pending_identity(context, PendingCause::Competing, &expected);
            let ServeMachine::Pending { name } = &context.machine else {
                panic!("ambiguous identity cannot establish hostname")
            };
            pending(name, PendingCause::Missing, &[]);
            assert_eq!(
                context.persistence,
                ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
            );
        });
        let args = if mask & 1 != 0 {
            vec!["serve", "--token", "private-same"]
        } else {
            vec!["serve"]
        };
        assert_eq!(run(&args, &mut backend).0, 3);
        consumed(backend);
    }
}

#[test]
fn name_with_environment_token_remains_pending() {
    let mut facts = inputs();
    facts.environment_token = private("private-token");
    let mut backend = fixture(facts, |context| {
        pending_identity(
            context,
            PendingCause::OwnerPolicy,
            &[(Origin::EnvironmentToken, "private-token")],
        );
        let ServeMachine::Pending { name } = &context.machine else {
            panic!("expected unresolved name")
        };
        pending(
            name,
            PendingCause::OwnerPolicy,
            &[(ProfileOrigin::Explicit, "chosen")],
        );
    });
    assert_eq!(run(&["serve", "chosen"], &mut backend).0, 3);
    consumed(backend);
}

#[test]
fn absent_and_empty_credentials_never_enable_hostname_fallback() {
    for origin in [
        None,
        Some(Origin::ExplicitToken),
        Some(Origin::EnvironmentToken),
        Some(Origin::SavedWorkerIdentity),
        Some(Origin::SelectedLogin),
    ] {
        let mut facts = inputs();
        match origin {
            Some(Origin::EnvironmentToken) => facts.environment_token = private(""),
            Some(Origin::SavedWorkerIdentity) => facts.saved_worker_identity = private(""),
            Some(Origin::SelectedLogin) => facts.selected_login = private(""),
            _ => {}
        }
        let mut backend = fixture(facts, move |context| {
            let expected = origin.map(|o| vec![(o, "")]).unwrap_or_default();
            pending_identity(
                context,
                if origin.is_some() {
                    PendingCause::OwnerPolicy
                } else {
                    PendingCause::Missing
                },
                &expected,
            );
            let ServeMachine::Pending { name } = &context.machine else {
                panic!("no justified hostname")
            };
            pending(name, PendingCause::Missing, &[]);
        });
        let args = if origin == Some(Origin::ExplicitToken) {
            vec!["serve", "--token", ""]
        } else {
            vec!["serve"]
        };
        assert_eq!(run(&args, &mut backend).0, 3);
        consumed(backend);
    }
}

#[test]
fn empty_explicit_scalars_retain_defaults_without_selecting_them() {
    let mut facts = inputs();
    facts.selected_login = private("private-login");
    let mut backend = fixture(facts, |context| {
        let ServeMachine::Named(name) = &context.machine else {
            panic!("expected login name intent")
        };
        pending(
            name,
            PendingCause::OwnerPolicy,
            &[
                (ProfileOrigin::Explicit, ""),
                (ProfileOrigin::Hostname, "injected-host"),
            ],
        );
        pending(
            &context.directory,
            PendingCause::OwnerPolicy,
            &[
                (ProfileOrigin::Explicit, ""),
                (ProfileOrigin::Cwd, "/injected/work"),
            ],
        );
    });
    // Backend scalar contract; this does not claim Clap accepts an empty PathBuf.
    backend
        .execute(request(json!({"name":"","dir":""})))
        .unwrap();
    consumed(backend);
    for empty in [false, true] {
        let mut facts = ServeInputs {
            selected_login: private("private-login"),
            ..Default::default()
        };
        if empty {
            facts.hostname = Some(String::new());
            facts.cwd = Some(String::new());
        }
        let mut backend = fixture(facts, move |context| {
            let ServeMachine::Named(name) = &context.machine else {
                panic!("expected login name intent")
            };
            let cause = if empty {
                PendingCause::OwnerPolicy
            } else {
                PendingCause::Missing
            };
            pending(
                name,
                cause,
                &if empty {
                    vec![(ProfileOrigin::Hostname, "")]
                } else {
                    vec![]
                },
            );
            pending(
                &context.directory,
                cause,
                &if empty {
                    vec![(ProfileOrigin::Cwd, "")]
                } else {
                    vec![]
                },
            );
        });
        run(&["serve"], &mut backend);
        consumed(backend);
    }
}
