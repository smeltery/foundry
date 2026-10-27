mod support_routes;
use serde_json::{Value, json};
use support_routes::*;

#[test]
fn network_plugin_and_configuration_routes_have_explicit_nested_requests() {
    for (argv, expected) in [
        (
            vec!["network"],
            op(
                "network",
                json!({"command":null,"address":null,"message":null}),
            ),
        ),
        (
            vec!["network", "team/agent", "  question λ\n"],
            op(
                "network",
                json!({"command":null,"address":"team/agent","message":"  question λ\n"}),
            ),
        ),
        (
            vec!["network", "who"],
            op(
                "network",
                json!({"command":op("who", json!({"filter":null,"all":false,"json":false})),"address":null,"message":null}),
            ),
        ),
        (
            vec!["network", "serve", "adapter"],
            op(
                "network",
                json!({"command":op("serve", json!({"agent":"adapter"})),"address":null,"message":null}),
            ),
        ),
        (
            vec!["plugins", "list"],
            op(
                "plugins",
                op(
                    "list",
                    json!({"available":false,"client":null,"scope":"user","project":null}),
                ),
            ),
        ),
        (
            vec!["plugins", "add", "package", "--client", "codex"],
            op(
                "plugins",
                op(
                    "add",
                    json!({"name":"package","version":null,"client":"codex","scope":"user","project":null,"replace":false,"dry_run":false,"yes":false}),
                ),
            ),
        ),
        (
            vec!["plugins", "remove", "package", "--client", "codex"],
            op(
                "plugins",
                op(
                    "remove",
                    json!({"name":"package","client":"codex","scope":"user","project":null,"dry_run":false,"yes":false}),
                ),
            ),
        ),
        (vec!["config", "list"], op("config", unit("list"))),
        (
            vec!["config", "get", "key"],
            op("config", op("get", json!({"key":"key"}))),
        ),
        (
            vec!["config", "set", "key", "  value λ\n"],
            op(
                "config",
                op("set", json!({"key":"key","value":"  value λ\n"})),
            ),
        ),
        (
            vec!["config", "unset", "key"],
            op("config", op("unset", json!({"key":"key"}))),
        ),
    ] {
        route(&argv, expected);
    }
}

#[test]
fn connection_worker_and_queue_routes_preserve_defaults_and_explicit_settings() {
    for command in ["connect", "device"] {
        route(&[command], op("connect", connect_options(json!({}))));
        route(
            &[
                command,
                "--to",
                "worker",
                "--api-key",
                "fixture-key",
                "--api-url",
                "https://example.invalid",
                "--name",
                "computer",
                "--folder",
                "/workspace",
                "--add-folder",
                "/extra-a",
                "--add-folder",
                "/extra-b",
                "--remove-folder",
                "/old",
                "--proxy",
                "http://proxy.invalid",
                "--proxy-ca-file",
                "/ca.pem",
                "--user-chrome",
                "off",
                "--local-logins",
                "on",
                "--keep-awake",
                "off",
                "--once",
                "--yes",
                "--minimal",
            ],
            op(
                "connect",
                connect_options(
                    json!({"to":"worker","api_key":"fixture-key","api_url":"https://example.invalid",
                "name":"computer","folder":"/workspace","add_folder":["/extra-a","/extra-b"],"remove_folder":["/old"],
                "proxy":"http://proxy.invalid","proxy_ca_file":"/ca.pem","user_chrome":"off","local_logins":"on",
                "keep_awake":"off","once":true,"yes":true,"minimal":true}),
                ),
            ),
        );
        route(
            &[
                command,
                "--token",
                "fixture-token",
                "--user-chrome",
                "on",
                "--local-logins",
                "off",
                "--keep-awake",
                "on",
            ],
            op(
                "connect",
                connect_options(
                    json!({"token":"fixture-token","user_chrome":"on","local_logins":"off","keep_awake":"on"}),
                ),
            ),
        );
    }
    route(&["serve"], op("serve", serve_options(json!({}))));
    route(
        &[
            "serve",
            "worker",
            "--dir",
            "/workspace",
            "--service",
            "--sandbox",
            "--yes",
            "--stop",
        ],
        op(
            "serve",
            serve_options(
                json!({"name":"worker","dir":"/workspace","service":true,"sandbox":true,"yes":true,"stop":true}),
            ),
        ),
    );
    route(
        &["serve", "--token", "fixture-token", "--once"],
        op(
            "serve",
            serve_options(json!({"token":"fixture-token","once":true})),
        ),
    );
    for (argv, expected) in [
        (
            vec!["disconnect"],
            op("disconnect", json!({"name":null,"yes":false})),
        ),
        (
            vec!["disconnect", "worker", "--yes"],
            op("disconnect", json!({"name":"worker","yes":true})),
        ),
        (vec!["stop", "worker"], op("stop", json!({"name":"worker"}))),
        (vec!["machines"], op("machines", json!({"command":null}))),
        (
            vec!["machines", "add", "worker"],
            op(
                "machines",
                json!({"command":op("add", json!({"name":"worker"}))}),
            ),
        ),
        (
            vec!["machines", "token", "worker"],
            op(
                "machines",
                json!({"command":op("token", json!({"name":"worker"}))}),
            ),
        ),
        (
            vec!["machines", "rm", "worker"],
            op(
                "machines",
                json!({"command":op("rm", json!({"name":"worker","yes":false}))}),
            ),
        ),
        (
            vec!["machines", "rm", "worker", "--yes"],
            op(
                "machines",
                json!({"command":op("rm", json!({"name":"worker","yes":true}))}),
            ),
        ),
    ] {
        route(&argv, expected);
    }
}

#[test]
fn desktop_permissions_accounts_and_update_routes_preserve_exact_shapes() {
    route(
        &["desktop", "install"],
        op("desktop", op("install", json!({"permissions":"none"}))),
    );
    for level in ["none", "standard", "all"] {
        route(
            &["desktop", "install", "--permissions", level],
            op("desktop", op("install", json!({"permissions":level}))),
        );
    }
    for (command, operation) in [
        ("minimal", "minimal"),
        ("status", "status"),
        ("browser-use", "browser_use"),
        ("mentions", "mentions"),
    ] {
        route(&["desktop", command], op("desktop", unit(operation)));
    }
    route(&["permissions"], op("permissions", json!({"command":null})));
    route(
        &["permissions", "allow"],
        op(
            "permissions",
            json!({"command":op("allow", json!({"permission":null}))}),
        ),
    );
    for permission in ["screen", "accessibility", "disk"] {
        route(
            &["permissions", "allow", permission],
            op(
                "permissions",
                json!({"command":op("allow", json!({"permission":permission}))}),
            ),
        );
    }
    for command in ["login", "logout", "whoami", "status", "mcp"] {
        route(&[command], unit(command));
    }
    for command in ["update", "upgrade", "--upgrade"] {
        route(
            &[command],
            op("update", json!({"check":false,"quiet":false})),
        );
    }
    for command in ["update", "upgrade"] {
        route(
            &[command, "--check", "--quiet"],
            op("update", json!({"check":true,"quiet":true})),
        );
    }
    for (flags, pair, browser, token_stdin) in [
        (vec![], None, false, false),
        (
            vec!["--pair", "fixture-code"],
            Some("fixture-code"),
            false,
            false,
        ),
        (vec!["--browser"], None, true, false),
        (vec!["--token-stdin"], None, false, true),
    ] {
        let mut argv = vec!["providers", "connect", "fixture-provider"];
        argv.extend(flags);
        route(
            &argv,
            op(
                "providers",
                op(
                    "connect",
                    json!({"provider":"fixture-provider","pair":pair,"browser":browser,"token_stdin":token_stdin}),
                ),
            ),
        );
    }
}

pub fn connect_options(explicit: Value) -> Value {
    fields(
        json!({
            "to":null,"token":null,"api_key":null,"api_url":null,"once":false,"yes":false,
            "name":null,"folder":null,"add_folder":[],"remove_folder":[],"proxy":null,
            "proxy_ca_file":null,"user_chrome":null,"local_logins":null,"keep_awake":null,"minimal":false
        }),
        explicit,
    )
}
pub fn serve_options(explicit: Value) -> Value {
    fields(
        json!({"name":null,"token":null,"dir":null,"once":false,"service":false,"sandbox":false,"yes":false,"stop":false}),
        explicit,
    )
}

fn unit(operation: &str) -> Value {
    json!({"operation":operation})
}
