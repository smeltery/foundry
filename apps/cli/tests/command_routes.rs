mod support_routes;
use serde_json::{Value, json};
use support_routes::*;

#[test]
fn task_and_editor_routes_preserve_complete_arguments_and_absent_grants() {
    route(
        &[],
        op(
            "prompt",
            json!({"prompt":null,"options":prompt_options(json!({}))}),
        ),
    );
    route(
        &["  task λ\n"],
        op(
            "prompt",
            json!({"prompt":"  task λ\n","options":prompt_options(json!({}))}),
        ),
    );
    let targeted = prompt_options(json!({
        "on":"worker","dir":"/workspace","yes":true,"detach":true,"repo":"team/repo",
        "model":"model","resume":"session-id","update":true
    }));
    route(
        &[
            "task",
            "--on",
            "worker",
            "--dir",
            "/workspace",
            "-y",
            "-d",
            "--repo",
            "team/repo",
            "--model",
            "model",
            "--resume",
            "session-id",
            "--update",
        ],
        op("prompt", json!({"prompt":"task","options":targeted})),
    );
    route(
        &["task", "--cloud", "--no-repo"],
        op(
            "prompt",
            json!({
                "prompt":"task","options":prompt_options(json!({"cloud":true,"no_repo":true}))
            }),
        ),
    );
    route(
        &["run", "--task", "task"],
        op("run", run_options(json!({}))),
    );
    route(
        &[
            "run",
            "--task",
            "  task λ\n",
            "--repo",
            "team/repo",
            "--ref",
            "branch",
            "--model",
            "model",
            "-d",
        ],
        op(
            "run",
            run_options(
                json!({"task":"  task λ\n","repo":"team/repo","git_ref":"branch","model":"model","detach":true}),
            ),
        ),
    );
    route(
        &["session", "start", "--prompt", "task"],
        op("session", op("start", start_options(json!({})))),
    );
    for provider in ["github", "gitlab"] {
        route(
            &[
                "session",
                "start",
                "--prompt",
                "  task λ\n",
                "--repo",
                "team/repo",
                "--provider",
                provider,
                "--model",
                "model",
            ],
            op(
                "session",
                op(
                    "start",
                    start_options(
                        json!({"prompt":"  task λ\n","repo":"team/repo","provider":provider,"model":"model"}),
                    ),
                ),
            ),
        );
    }
    route(&["acp"], op("acp", prompt_options(json!({}))));
    route(
        &[
            "acp",
            "--on",
            "worker",
            "--dir",
            "/workspace",
            "-y",
            "-d",
            "--repo",
            "team/repo",
            "--model",
            "model",
            "--resume",
            "session-id",
            "--update",
        ],
        op("acp", targeted),
    );
    // Existing ACP option serialization only; no editor/updater execution claim.
    route(
        &["acp", "--cloud", "--no-repo", "--upgrade"],
        op(
            "acp",
            prompt_options(json!({"cloud":true,"no_repo":true,"upgrade":true})),
        ),
    );
}

#[test]
fn session_routes_and_root_shortcuts_preserve_their_distinct_wrappers() {
    for root in [true, false] {
        let wrap = |name, args| {
            if root {
                op(name, args)
            } else {
                op("session", op(name, args))
            }
        };
        let list = if root {
            vec!["ls"]
        } else {
            vec!["session", "list"]
        };
        let list_name = if root { "ls" } else { "list" };
        route(&list, wrap(list_name, list_options(json!({}))));
        for status in ["active", "archived", "all"] {
            let mut argv = list.clone();
            argv.extend([
                "--limit",
                "7",
                "--status",
                status,
                "--origin",
                "external:cli",
                "--fields",
                "title,status",
                "--fields",
                "model",
            ]);
            route(
                &argv,
                wrap(
                    list_name,
                    list_options(
                        json!({"limit":7,"status":status,"origin":"external:cli","fields":["title","status","model"]}),
                    ),
                ),
            );
        }
        for (name, argv, expected) in [
            (
                "watch",
                vec!["watch", "session-id"],
                json!({"id":"session-id","interval":null}),
            ),
            (
                "watch",
                vec!["watch", "session-id", "--interval", "9"],
                json!({"id":"session-id","interval":9}),
            ),
            (
                "cancel",
                vec!["cancel", "session-id"],
                json!({"id":"session-id","hard":false}),
            ),
            (
                "cancel",
                vec!["cancel", "session-id", "--hard"],
                json!({"id":"session-id","hard":true}),
            ),
        ] {
            let args = if root {
                argv
            } else {
                std::iter::once("session").chain(argv).collect()
            };
            route(&args, wrap(name, expected));
        }
        let send = if root {
            vec!["send", "session-id", "  follow-up λ\n"]
        } else {
            vec![
                "session",
                "send",
                "session-id",
                "--message",
                "  follow-up λ\n",
            ]
        };
        route(
            &send,
            wrap(
                "send",
                json!({"id":"session-id","message":"  follow-up λ\n"}),
            ),
        );
    }
    for origin in ["ui", "external", "external:mcp", "external:api"] {
        route(
            &["session", "list", "--origin", origin],
            op(
                "session",
                op("list", list_options(json!({"origin":origin}))),
            ),
        );
    }
    for (argv, expected) in [
        (
            vec!["get", "session-id"],
            op("get", json!({"id":"session-id"})),
        ),
        (
            vec!["attachments", "session-id"],
            op("attachments", json!({"id":"session-id"})),
        ),
        (
            vec!["download", "session-id"],
            op(
                "download",
                json!({"id":"session-id","attachment":null,"output":null,"force":false}),
            ),
        ),
        (
            vec![
                "download",
                "session-id",
                "artifact",
                "--output",
                "report.txt",
                "--force",
            ],
            op(
                "download",
                json!({"id":"session-id","attachment":"artifact","output":"report.txt","force":true}),
            ),
        ),
        (
            vec!["share-link", "session-id"],
            op(
                "share_link",
                json!({"id":"session-id","label":null,"expires_in":null}),
            ),
        ),
        (
            vec![
                "share-link",
                "session-id",
                "--label",
                "report λ",
                "--expires-in",
                "90",
            ],
            op(
                "share_link",
                json!({"id":"session-id","label":"report λ","expires_in":90}),
            ),
        ),
    ] {
        let argv: Vec<_> = std::iter::once("session").chain(argv).collect();
        route(&argv, op("session", expected));
    }
}

#[test]
fn global_database_and_output_selectors_never_leak_into_backend_requests() {
    for argv in [
        vec![
            "--database",
            "fixture.db",
            "--json",
            "session",
            "get",
            "session-id",
        ],
        vec![
            "session",
            "get",
            "session-id",
            "--database",
            "fixture.db",
            "--output-format",
            "json",
        ],
        vec!["session", "get", "session-id", "--output-format=text"],
    ] {
        route(&argv, op("session", op("get", json!({"id":"session-id"}))));
    }
}
pub fn prompt_options(explicit: Value) -> Value {
    fields(
        json!({
            "cloud":false,"on":null,"dir":null,"yes":false,"detach":false,
            "repo":null,"no_repo":false,"model":null,"effort":null,"resume":null,"update":false,"upgrade":false
        }),
        explicit,
    )
}
pub fn run_options(explicit: Value) -> Value {
    fields(
        json!({"task":"task","repo":null,"git_ref":null,"model":null,"effort":null,"detach":false}),
        explicit,
    )
}
pub fn start_options(explicit: Value) -> Value {
    fields(
        json!({"prompt":"task","repo":null,"provider":null,"model":null,"effort":null}),
        explicit,
    )
}
pub fn list_options(explicit: Value) -> Value {
    fields(
        json!({"limit":null,"status":null,"origin":null,"fields":[]}),
        explicit,
    )
}
