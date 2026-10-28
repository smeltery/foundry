use serde_json::{Value, json};

fn op(operation: &str, arguments: Value) -> Value {
    json!({"operation":operation,"arguments":arguments})
}
pub fn routes() -> Vec<(Vec<&'static str>, Value)> {
    let mut cases = vec![
        (
            vec!["task"],
            op(
                "prompt",
                json!({"prompt":"task","options":{
                    "cloud":false,"on":null,"dir":null,"yes":false,"detach":false,"repo":null,
                    "no_repo":false,"model":null,"effort":null,"resume":null,"update":false,"upgrade":false
                }}),
            ),
        ),
        (
            vec!["run", "--task", "task"],
            op(
                "run",
                json!({"task":"task","repo":null,"git_ref":null,"model":null,"effort":null,"detach":false}),
            ),
        ),
        (
            vec!["session", "start", "--prompt", "task"],
            op(
                "session",
                op(
                    "start",
                    json!({"prompt":"task","repo":null,"provider":null,"model":null,"effort":null}),
                ),
            ),
        ),
        (
            vec!["session", "get", "id"],
            op("session", op("get", json!({"id":"id"}))),
        ),
        (
            vec!["session", "attachments", "id"],
            op("session", op("attachments", json!({"id":"id"}))),
        ),
        (
            vec!["session", "download", "id"],
            op(
                "session",
                op(
                    "download",
                    json!({"id":"id","attachment":null,"output":null,"force":false}),
                ),
            ),
        ),
        (
            vec!["session", "share-link", "id"],
            op(
                "session",
                op(
                    "share_link",
                    json!({"id":"id","label":null,"expires_in":null}),
                ),
            ),
        ),
    ];
    for nested in [false, true] {
        for (root, name, args, expected) in [
            (
                "ls",
                "list",
                vec![],
                json!({"limit":null,"status":null,"origin":null,"fields":[]}),
            ),
            (
                "watch",
                "watch",
                vec!["id"],
                json!({"id":"id","interval":null}),
            ),
            (
                "send",
                "send",
                vec!["id", "message"],
                json!({"id":"id","message":"message"}),
            ),
            (
                "cancel",
                "cancel",
                vec!["id"],
                json!({"id":"id","hard":false}),
            ),
        ] {
            let mut argv = if nested {
                vec!["session", name]
            } else {
                vec![root]
            };
            if nested && name == "send" {
                argv.extend(["id", "--message", "message"]);
            } else {
                argv.extend(args);
            }
            let command = if nested {
                op("session", op(name, expected))
            } else {
                op(root, expected)
            };
            cases.push((argv, command));
        }
    }
    cases
}

pub fn explicit_tasks() -> Vec<(Vec<&'static str>, Value)> {
    vec![
        (
            vec![
                " task λ ",
                "--on",
                "worker",
                "--dir",
                "/workspace",
                "--repo",
                "team/repo",
                "--model",
                "model",
                "--effort",
                "high",
                "--resume",
                "session-id",
                "--update",
                "--yes",
                "--detach",
                "--tool-grant",
                "read",
            ],
            op(
                "prompt",
                json!({"prompt":" task λ ","options":{
                    "cloud":false,"on":"worker","dir":"/workspace","yes":true,"detach":true,"repo":"team/repo",
                    "no_repo":false,"model":"model","effort":"high","resume":"session-id","update":true,"upgrade":false,"tool_grants":["read"]
                }}),
            ),
        ),
        (
            vec!["task", "--cloud", "--no-repo"],
            op(
                "prompt",
                json!({"prompt":"task","options":{
                    "cloud":true,"on":null,"dir":null,"yes":false,"detach":false,"repo":null,
                    "no_repo":true,"model":null,"effort":null,"resume":null,"update":false,"upgrade":false
                }}),
            ),
        ),
        (
            vec![
                "run",
                "--task",
                " task λ ",
                "--repo",
                "team/repo",
                "--ref",
                "branch",
                "--model",
                "model",
                "--effort",
                "xhigh",
                "--detach",
            ],
            op(
                "run",
                json!({"task":" task λ ","repo":"team/repo","git_ref":"branch","model":"model","effort":"xhigh","detach":true}),
            ),
        ),
        (
            vec![
                "session",
                "start",
                "--prompt",
                " task λ ",
                "--repo",
                "team/repo",
                "--provider",
                "gitlab",
                "--model",
                "model",
                "--effort",
                "low",
            ],
            op(
                "session",
                op(
                    "start",
                    json!({"prompt":" task λ ","repo":"team/repo","provider":"gitlab","model":"model","effort":"low"}),
                ),
            ),
        ),
    ]
}
