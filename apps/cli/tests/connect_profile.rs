mod support_connect_profile;
use ProfileOrigin::*;
use f5y::{Request, configuration::*};
use serde_json::json;
use support_connect_profile::*;

#[test]
fn fresh_defaults_are_consumed_from_injected_personal_machine_and_auth_facts() {
    for command in ["connect", "device"] {
        for (kind, origin, folder) in [
            (ProfileKind::Personal, Home, "/injected/home"),
            (ProfileKind::Machine, Cwd, "/injected/cwd"),
        ] {
            for key in [false, true] {
                let mut f = facts(ProfileLifecycle::Fresh);
                f.kind = kind;
                let (request, p, accounts) = run(&[command], inputs(f, key));
                assert_eq!(p.name, selected(Hostname, "injected-host".into()));
                assert_eq!(p.folder, selected(origin, folder.into()));
                assert_eq!(p.chrome, selected(PlatformDefault, true));
                assert_eq!(p.awake, selected(PlatformDefault, true));
                assert_eq!(p.logins, selected(AuthenticationDefault, !key));
                assert_eq!(p.ca, missing());
                assert_eq!(p.minimal, missing());
                assert_eq!(
                    accounts,
                    if key {
                        vec![Origin::EnvironmentKey]
                    } else {
                        vec![]
                    }
                );
                no_changes(&p);
                assert_eq!(request["command"]["operation"], "connect");
                for field in [
                    "name",
                    "folder",
                    "user_chrome",
                    "local_logins",
                    "keep_awake",
                ] {
                    assert!(request["command"]["arguments"][field].is_null());
                }
                assert!(!request.to_string().contains("injected-"));
                assert!(!request.to_string().contains("/injected/"));
            }
        }
    }
}

#[test]
fn retained_settings_survive_omission_and_documented_nonempty_changes_are_distinct() {
    let (_, p, _) = run(
        &["connect"],
        inputs(facts(ProfileLifecycle::Retained(saved())), false),
    );
    assert_eq!(p.name, selected(Retained, "saved name".into()));
    assert_eq!(p.folder, selected(Retained, "../saved folder".into()));
    assert_eq!(p.ca, selected(Retained, "/missing/ca.pem".into()));
    assert_eq!(p.chrome, selected(Retained, false));
    assert_eq!(p.logins, selected(Retained, false));
    assert_eq!(p.awake, selected(Retained, false));
    assert_eq!(p.minimal, selected(Retained, true));
    no_changes(&p);
    let (request, p, _) = run(
        &[
            "device",
            "--name",
            " new λ ",
            "--folder",
            "./new folder",
            "--proxy-ca-file",
            "/absent/new.pem",
            "--user-chrome",
            "on",
            "--local-logins",
            "on",
            "--keep-awake",
            "on",
        ],
        inputs(facts(ProfileLifecycle::Retained(saved())), false),
    );
    assert_eq!(p.name, selected(Explicit, " new λ ".into()));
    assert_eq!(p.folder, selected(Explicit, "./new folder".into()));
    assert_eq!(p.ca, selected(Explicit, "/absent/new.pem".into()));
    assert_eq!(p.chrome, selected(Explicit, true));
    assert_eq!(p.logins, selected(Explicit, true));
    assert_eq!(p.awake, selected(Explicit, true));
    assert_eq!(p.minimal, selected(Retained, true));
    assert_eq!(p.changes.name.as_deref(), Some(" new λ "));
    assert_eq!(p.changes.folder.as_deref(), Some("./new folder"));
    assert_eq!(p.changes.proxy_ca_file.as_deref(), Some("/absent/new.pem"));
    assert_eq!(p.changes.user_chrome, Some(true));
    assert_eq!(p.changes.local_logins, Some(true));
    assert_eq!(p.changes.keep_awake, Some(true));
    assert!(!p.changes.minimal && p.immediate_awake && !p.immediate_folders);
    assert!(p.preserves_sessions && p.preserves_desktop);
    assert_eq!(request["command"]["arguments"]["folder"], "./new folder");
}

#[test]
fn immediate_folder_awake_and_minimal_intents_do_not_erase_other_state() {
    let (request, p, _) = run(
        &[
            "connect",
            "--minimal",
            "--keep-awake",
            "off",
            "--user-chrome",
            "on",
            "--add-folder",
            "./same",
            "--add-folder",
            "./same",
            "--add-folder",
            "../other λ",
            "--remove-folder",
            "./same",
            "--remove-folder",
            "../old",
        ],
        inputs(facts(ProfileLifecycle::Fresh), false),
    );
    assert_eq!(p.minimal, selected(Explicit, true));
    assert_eq!(p.chrome, selected(Explicit, true));
    assert_eq!(p.awake, selected(Explicit, false));
    assert!(p.changes.minimal && p.immediate_awake && p.immediate_folders);
    assert_eq!(p.changes.keep_awake, Some(false));
    assert_eq!(p.changes.add_folder, ["./same", "./same", "../other λ"]);
    assert_eq!(p.changes.remove_folder, ["./same", "../old"]);
    assert!(p.preserves_sessions && p.preserves_desktop);
    assert_eq!(
        request["command"]["arguments"]["add_folder"],
        json!(["./same", "./same", "../other λ"])
    );
    assert_eq!(
        request["command"]["arguments"]["remove_folder"],
        json!(["./same", "../old"])
    );
    // No default Chrome preference is disabled merely by minimal intent.
    let (_, p, _) = run(
        &["connect", "--minimal"],
        inputs(facts(ProfileLifecycle::Fresh), false),
    );
    assert_eq!(p.chrome, selected(PlatformDefault, true));
    assert_eq!(p.minimal, selected(Explicit, true));
    assert!(p.preserves_desktop);
}

#[test]
fn missing_facts_retained_absence_and_unknown_platform_do_not_invent_defaults() {
    for input in [
        ConnectInputs::default(),
        inputs(
            facts(ProfileLifecycle::Retained(SavedProfile::default())),
            false,
        ),
    ] {
        let (_, p, _) = run(&["connect"], input);
        assert_eq!(p.name, missing());
        assert_eq!(p.folder, missing());
        assert_eq!(p.ca, missing());
        assert_eq!(p.chrome, missing());
        assert_eq!(p.awake, missing());
        assert_eq!(p.logins, missing());
        assert_eq!(p.minimal, missing());
        no_changes(&p);
    }
    for platform in [ProfilePlatform::Other, ProfilePlatform::Unknown] {
        let mut f = facts(ProfileLifecycle::Fresh);
        f.platform = platform;
        f.kind = ProfileKind::Unknown;
        f.hostname = None;
        let (_, p, _) = run(&["connect"], inputs(f, false));
        assert_eq!(p.name, missing());
        assert_eq!(p.folder, missing());
        assert_eq!(p.chrome, missing());
        assert_eq!(p.awake, missing());
        let mut f = facts(ProfileLifecycle::Retained(saved()));
        f.platform = platform;
        let (_, p, _) = run(&["connect"], inputs(f, false));
        assert_eq!(p.chrome, pending(Retained, false));
        assert_eq!(p.awake, pending(Retained, false));
        let mut f = facts(ProfileLifecycle::Fresh);
        f.platform = platform;
        let (_, p, _) = run(
            &["connect", "--user-chrome", "on", "--keep-awake", "off"],
            inputs(f, false),
        );
        assert_eq!(p.chrome, pending(Explicit, true));
        assert_eq!(p.awake, pending(Explicit, false));
        assert_eq!(p.changes.user_chrome, Some(true));
        assert_eq!(p.changes.keep_awake, Some(false));
    }
}

#[test]
fn key_and_unknown_authentication_leave_login_on_unresolved() {
    for retained in [false, true] {
        let mut saved = saved();
        saved.local_logins = Some(true);
        let lifecycle = if retained {
            ProfileLifecycle::Retained(saved)
        } else {
            ProfileLifecycle::Fresh
        };
        let argv = if retained {
            vec!["connect"]
        } else {
            vec!["connect", "--local-logins", "on"]
        };
        let (_, p, accounts) = run(&argv, inputs(facts(lifecycle), true));
        assert_eq!(
            p.logins,
            pending(if retained { Retained } else { Explicit }, true)
        );
        assert_eq!(accounts, [Origin::EnvironmentKey]);
    }
    for explicit in [false, true] {
        let mut f = facts(ProfileLifecycle::Fresh);
        f.authentication = ProfileAuthentication::Unknown;
        let argv = if explicit {
            vec!["connect", "--local-logins", "on"]
        } else {
            vec!["connect"]
        };
        let (_, p, _) = run(&argv, inputs(f, false));
        assert_eq!(
            p.logins,
            if explicit {
                pending(Explicit, true)
            } else {
                missing()
            }
        );
    }
    for input in [
        ConnectInputs {
            environment_key: Some(PrivateValue::new("")),
            ..inputs(facts(ProfileLifecycle::Fresh), false)
        },
        ConnectInputs {
            saved_identity: Some(PrivateValue::new("saved-identity")),
            ..inputs(facts(ProfileLifecycle::Fresh), false)
        },
    ] {
        let (_, p, accounts) = run(&["connect"], input);
        assert_eq!(p.logins, missing());
        assert!(accounts.is_empty());
    }
    let (_, p, accounts) = run(
        &["connect", "--api-key", "other-key", "--local-logins", "on"],
        inputs(facts(ProfileLifecycle::Fresh), true),
    );
    assert_eq!(p.logins, pending(Explicit, true));
    assert!(accounts.is_empty());
    let mut f = facts(ProfileLifecycle::Fresh);
    f.authentication = ProfileAuthentication::Unknown;
    let (_, p, _) = run(&["connect", "--local-logins", "off"], inputs(f, false));
    assert_eq!(p.logins, selected(Explicit, false));
}

#[test]
fn empty_scalar_changes_preserve_retained_and_fresh_candidates_without_selecting() {
    let (_, p, _) = run_empty(inputs(facts(ProfileLifecycle::Retained(saved())), false));
    for (value, original) in [
        (p.name, "saved name"),
        (p.folder, "../saved folder"),
        (p.ca, "/missing/ca.pem"),
    ] {
        assert_eq!(
            value,
            Observed::Pending(
                PendingCause::OwnerPolicy,
                vec![(Explicit, "".into()), (Retained, original.into())]
            )
        );
    }
    assert_eq!(p.changes.name.as_deref(), Some(""));
    assert_eq!(p.changes.folder.as_deref(), Some(""));
    assert_eq!(p.changes.proxy_ca_file.as_deref(), Some(""));
    for (kind, origin, folder) in [
        (ProfileKind::Personal, Home, "/injected/home"),
        (ProfileKind::Machine, Cwd, "/injected/cwd"),
    ] {
        let mut f = facts(ProfileLifecycle::Fresh);
        f.kind = kind;
        let (_, p, _) = run_empty(inputs(f, false));
        assert_eq!(
            p.name,
            Observed::Pending(
                PendingCause::OwnerPolicy,
                vec![(Explicit, "".into()), (Hostname, "injected-host".into())]
            )
        );
        assert_eq!(
            p.folder,
            Observed::Pending(
                PendingCause::OwnerPolicy,
                vec![(Explicit, "".into()), (origin, folder.into())]
            )
        );
        assert_eq!(p.ca, pending(Explicit, "".into()));
    }
    let (_, p, _) = run_empty(inputs(
        facts(ProfileLifecycle::Retained(SavedProfile::default())),
        false,
    ));
    assert_eq!(p.name, pending(Explicit, "".into()));
    assert_eq!(p.folder, pending(Explicit, "".into()));
    assert_eq!(p.ca, pending(Explicit, "".into()));
}

#[test]
fn malformed_explicit_profile_fields_reject_before_injected_inputs() {
    for (fields, invalid) in [
        (
            vec!["name", "folder", "proxy_ca_file"],
            vec![json!(true), json!([]), json!({})],
        ),
        (
            vec!["user_chrome", "local_logins", "keep_awake"],
            vec![json!(true), json!("ON"), json!(""), json!({})],
        ),
        (vec!["minimal"], vec![json!("true"), json!(1), json!([])]),
        (
            vec!["add_folder", "remove_folder"],
            vec![json!("path"), json!(["path", false]), json!({})],
        ),
    ] {
        for field in fields {
            for value in &invalid {
                let mut args = json!({});
                args[field] = value.clone();
                reject(Request {
                    version: 1,
                    command: json!({"operation":"connect","arguments":args}),
                });
            }
        }
    }
}
