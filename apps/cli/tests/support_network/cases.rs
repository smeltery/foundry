use serde_json::{Value, json};

pub type Case = (Vec<&'static str>, Value);

fn operation(name: &str, arguments: Value) -> Value {
    json!({"operation":name,"arguments":arguments})
}

// Literal request expectations, independent of parser serialization.
pub fn defaults() -> Vec<Case> {
    let mut cases = vec![
        (vec![], Value::Null),
        (
            vec!["who"],
            operation("who", json!({"filter":null,"all":false,"json":false})),
        ),
        (
            vec!["send", "person/session"],
            operation(
                "send",
                json!({"to":"person/session","message":[],"reply_to":null,"fyi":false,"notify":false,"from":null,"files":[]}),
            ),
        ),
        (
            vec!["status"],
            operation("status", json!({"doing":[],"name":null})),
        ),
        (
            vec!["init"],
            operation(
                "init",
                json!({"team":null,"person":null,"no_setup":false,"relay":null,"proof":null,"json":false}),
            ),
        ),
        (
            vec!["invite"],
            operation("invite", json!({"me":false,"code":false,"hours":24})),
        ),
        (
            vec!["link"],
            operation("link", json!({"new":false,"json":false})),
        ),
        (
            vec!["join"],
            operation(
                "join",
                json!({"invite":null,"person":null,"no_setup":false,"proof":null,"json":false}),
            ),
        ),
        (vec!["team"], operation("team", json!({"json":false}))),
        (
            vec!["proof", "fixture-proof"],
            operation("proof", json!({"jws":"fixture-proof"})),
        ),
        (
            vec!["remove", "person/machine"],
            operation("remove", json!({"who":"person/machine"})),
        ),
        (vec!["leave"], operation("leave", json!({"force":false}))),
        (
            vec!["relay", "default"],
            operation("relay", json!({"url":"default"})),
        ),
        (
            vec!["folder", "/workspace", "off"],
            operation("folder", json!({"path":"/workspace","state":"off"})),
        ),
        (
            vec!["approve", "message-id"],
            operation("approve", json!({"ids":["message-id"],"all":false})),
        ),
        (
            vec!["reject", "message-id"],
            operation("reject", json!({"ids":["message-id"],"all":false})),
        ),
        (
            vec!["setup"],
            operation(
                "setup",
                json!({"dry_run":false,"remove":false,"no_service":false}),
            ),
        ),
        (
            vec!["daemon"],
            operation("daemon", json!({"service":false})),
        ),
        (
            vec!["listen"],
            operation("listen", json!({"name":"agent","wait":600})),
        ),
        (
            vec!["hook", "fixture-agent", "fixture-event"],
            operation(
                "hook",
                json!({"agent":"fixture-agent","event":"fixture-event","session":null,"state":null,"doing":null}),
            ),
        ),
    ];
    for name in [
        "endpoint", "held", "doctor", "upgrade", "stop", "mcp", "bridge", "tray",
    ] {
        cases.push((vec![name], json!({"operation":name})));
    }
    cases
}

pub fn explicit() -> Vec<Case> {
    let mut cases = vec![
        (
            vec!["who", "  filter λ", "--all", "--json"],
            operation("who", json!({"filter":"  filter λ","all":true,"json":true})),
        ),
        (
            vec![
                "send",
                "person/session",
                "first",
                "  second λ\n",
                "--reply-to",
                "parent-id",
                "--fyi",
                "--from",
                "sender/session",
                "--file",
                "a.txt",
                "--file",
                "b.txt",
            ],
            operation(
                "send",
                json!({"to":"person/session","message":["first","  second λ\n"],"reply_to":"parent-id","fyi":true,"notify":false,"from":"sender/session","files":["a.txt","b.txt"]}),
            ),
        ),
        (
            vec!["send", "person", "-", "--notify"],
            operation(
                "send",
                json!({"to":"person","message":["-"],"reply_to":null,"fyi":false,"notify":true,"from":null,"files":[]}),
            ),
        ),
        (
            vec!["status", "first", "second", "--name", "worker"],
            operation(
                "status",
                json!({"doing":["first","second"],"name":"worker"}),
            ),
        ),
        (
            vec![
                "init",
                "team",
                "--as",
                "person",
                "--no-setup",
                "--relay",
                "https://relay.invalid",
                "--proof",
                "fixture-proof",
                "--json",
            ],
            operation(
                "init",
                json!({"team":"team","person":"person","no_setup":true,"relay":"https://relay.invalid","proof":"fixture-proof","json":true}),
            ),
        ),
        (
            vec!["invite", "--me", "--code", "--hours", "0"],
            operation("invite", json!({"me":true,"code":true,"hours":0})),
        ),
        (
            vec!["link", "--new", "--json"],
            operation("link", json!({"new":true,"json":true})),
        ),
        (
            vec![
                "join",
                "fixture-invite",
                "--as",
                "person",
                "--no-setup",
                "--proof",
                "fixture-proof",
                "--json",
            ],
            operation(
                "join",
                json!({"invite":"fixture-invite","person":"person","no_setup":true,"proof":"fixture-proof","json":true}),
            ),
        ),
        (
            vec!["team", "--json"],
            operation("team", json!({"json":true})),
        ),
        (
            vec!["leave", "--force"],
            operation("leave", json!({"force":true})),
        ),
        (
            vec!["relay", "https://relay.invalid"],
            operation("relay", json!({"url":"https://relay.invalid"})),
        ),
        (
            vec!["folder", "/workspace", "on"],
            operation("folder", json!({"path":"/workspace","state":"on"})),
        ),
        (
            vec!["setup", "--dry-run", "--remove", "--no-service"],
            operation(
                "setup",
                json!({"dry_run":true,"remove":true,"no_service":true}),
            ),
        ),
        (
            vec!["daemon", "--service"],
            operation("daemon", json!({"service":true})),
        ),
        (
            vec!["listen", "--as", "worker", "--wait", "0"],
            operation("listen", json!({"name":"worker","wait":0})),
        ),
        (
            vec![
                "hook",
                "fixture-agent",
                "fixture-event",
                "--session",
                "session-id",
                "--state",
                "fixture-state",
                "--doing",
                "  working λ\n",
            ],
            operation(
                "hook",
                json!({"agent":"fixture-agent","event":"fixture-event","session":"session-id","state":"fixture-state","doing":"  working λ\n"}),
            ),
        ),
    ];
    for name in ["approve", "reject"] {
        cases.push((
            vec![name, "--all"],
            operation(name, json!({"ids":[],"all":true})),
        ));
        cases.push((
            vec![name, "a", "b", "--all"],
            operation(name, json!({"ids":["a","b"],"all":true})),
        ));
    }
    cases.push((
        vec!["invite", "--hours", "18446744073709551615"],
        operation(
            "invite",
            json!({"me":false,"code":false,"hours":18446744073709551615u64}),
        ),
    ));
    cases.push((
        vec!["listen", "--wait", "18446744073709551615"],
        operation(
            "listen",
            json!({"name":"agent","wait":18446744073709551615u64}),
        ),
    ));
    cases
}
