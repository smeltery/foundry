use foundry_mcp::{Backend, McpError, PROTOCOL_VERSION, Server, list_tools, serve};
use serde_json::{Value, json};
use std::io::Cursor;

#[derive(Default)]
struct Recording(Vec<(String, Value)>);
impl Backend for Recording {
    fn call(&mut self, name: &str, args: Value) -> Result<Value, McpError> {
        self.0.push((name.into(), args.clone()));
        if args["session_id"] == "missing" {
            Err(McpError::new("not_found", "Session not found"))
        } else {
            Ok(json!({"tool":name,"arguments":args}))
        }
    }
}
fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn initialization(version: Value) -> Value {
    json!({"protocolVersion":version,"capabilities":{"elicitation":{"form":{},"url":{}}},
        "clientInfo":{"name":"test-client","title":"Test client","version":"1"},
        "_meta":{"progressToken":"init","com.example/context":{"opaque":true}}})
}
fn handle(server: &mut Server, backend: &mut Recording, method: &str, params: Value) -> Value {
    server
        .handle(
            request(json!(1), method, params).to_string().as_bytes(),
            backend,
        )
        .unwrap()
}

#[test]
fn both_versions_and_future_fallback_preserve_all_tools_over_stdio() {
    for (offered, selected) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2099-01-01", "2025-11-25"),
    ] {
        let cases = [
            (
                "session_create",
                json!({"prompt":"task 🦀","directory":"/tmp","idempotency_key":"key"}),
            ),
            ("session_list", json!({})),
            ("session_get", json!({"session_id":"s"})),
            (
                "session_send",
                json!({"session_id":"s","message":"next","message_id":"m"}),
            ),
            (
                "session_cancel",
                json!({"session_id":"s","mode":"graceful"}),
            ),
        ];
        let metadata = json!({"progressToken":7,"com.example/context":[null,false]});
        let mut input = vec![
            request(json!("init"), "initialize", initialization(json!(offered))),
            request(json!(1), "tools/list", json!({})),
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{"_meta":metadata}}),
            request(json!(2), "tools/list", json!({"_meta":metadata})),
        ];
        for (i, (name, args)) in cases.iter().enumerate() {
            input.push(request(
                json!(i + 3),
                "tools/call",
                json!({
                    "name":name,"arguments":args,"_meta":metadata
                }),
            ));
        }
        input.extend([
            request(
                json!(8),
                "tools/call",
                json!({"name":"session_get","arguments":{"session_id":"missing"},"_meta":metadata}),
            ),
            request(
                json!(9),
                "tools/call",
                json!({"name":"unknown","arguments":{}}),
            ),
            request(
                json!(10),
                "tools/call",
                json!({"name":"session_cancel","arguments":{"session_id":"s","mode":"invalid"}}),
            ),
            request(
                json!(11),
                "tools/call",
                json!({"name":"session_list","_meta":{"progressToken":false}}),
            ),
            request(json!(12), "initialize", initialization(json!(offered))),
            request(json!(13), "ping", json!({"_meta":metadata})),
        ]);
        let bytes: String = input
            .into_iter()
            .map(|frame| format!("{frame}\n"))
            .collect();
        let mut backend = Recording::default();
        let mut output = Vec::new();
        serve(&mut Cursor::new(bytes), &mut output, &mut backend).unwrap();
        let responses: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 14, "{offered}");
        assert_eq!(responses[0]["id"], "init");
        assert_eq!(
            responses[0]["result"],
            json!({
                "protocolVersion":selected,"capabilities":{"tools":{}},
                "serverInfo":{"name":"foundry-mcp","version":env!("CARGO_PKG_VERSION")}
            })
        );
        assert_eq!(responses[1]["error"]["code"], -32002);
        assert_eq!(responses[2]["result"]["tools"], list_tools());
        assert_eq!(responses[2]["result"]["tools"].as_array().unwrap().len(), 5);
        for (i, (name, args)) in cases.iter().enumerate() {
            assert_eq!(responses[i + 3]["id"], i + 3);
            assert_eq!(
                responses[i + 3]["result"],
                json!({"isError":false,
                "content":[{"type":"text","text":json!({"tool":name,"arguments":args}).to_string()}]})
            );
            assert_eq!(backend.0[i], ((*name).into(), args.clone()));
        }
        assert_eq!(
            responses[8]["result"],
            json!({"isError":true,
            "content":[{"type":"text","text":json!({"code":"not_found","message":"Session not found"}).to_string()}]})
        );
        for response in &responses[9..12] {
            assert_eq!(response["error"]["code"], -32602);
        }
        assert_eq!(responses[12]["error"]["code"], -32600);
        assert_eq!(responses[13]["result"], json!({}));
        assert_eq!(backend.0.len(), 6);
    }
}

#[test]
fn unsupported_string_selects_latest_without_unlocking_or_dispatching() {
    for offered in ["2024-11-05", "2099-01-01", "unsupported", ""] {
        let (mut server, mut backend) = (Server::default(), Recording::default());
        let response = handle(
            &mut server,
            &mut backend,
            "initialize",
            initialization(json!(offered)),
        );
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            handle(
                &mut server,
                &mut backend,
                "tools/call",
                json!({"name":"session_list"})
            )["error"]["code"],
            -32002
        );
        assert!(backend.0.is_empty());
        // A client unable to accept the selected version closes without initialized.
        let mut output = Vec::new();
        let input = format!(
            "{}\n",
            request(json!(1), "initialize", initialization(json!(offered)))
        );
        serve(&mut Cursor::new(input), &mut output, &mut backend).unwrap();
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(backend.0.is_empty());
    }
}

#[test]
fn malformed_version_does_not_consume_initialization() {
    for version in [json!(null), json!(true), json!(1), json!([]), json!({})] {
        let (mut server, mut backend) = (Server::default(), Recording::default());
        let mut params = initialization(version);
        for missing in [false, true] {
            if missing {
                params.as_object_mut().unwrap().remove("protocolVersion");
            }
            assert_eq!(
                handle(&mut server, &mut backend, "initialize", params.clone())["error"]["code"],
                -32602
            );
            assert!(
                server
                    .handle(
                        br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                        &mut backend
                    )
                    .is_none()
            );
            assert_eq!(
                handle(&mut server, &mut backend, "tools/list", json!({}))["error"]["code"],
                -32002
            );
        }
        assert_eq!(
            handle(
                &mut server,
                &mut backend,
                "initialize",
                initialization(json!("2025-06-18"))
            )["result"]["protocolVersion"],
            "2025-06-18"
        );
        assert!(backend.0.is_empty());
    }
}
