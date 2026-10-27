use foundry_mcp::{
    Backend, MAX_FRAME_BYTES, McpError, PROTOCOL_VERSION, Server, list_tools, serve,
};
use serde_json::{Value, json};
use std::io::Cursor;

#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    fail: bool,
}
impl Backend for Fake {
    fn call(&mut self, name: &str, args: Value) -> Result<Value, McpError> {
        self.calls.push((name.into(), args));
        if self.fail {
            Err(McpError::unsupported())
        } else {
            Ok(json!({"id":"session-1"}))
        }
    }
}
fn request(server: &mut Server, backend: &mut Fake, method: &str, params: Value) -> Value {
    server
        .handle(
            json!({"jsonrpc":"2.0","id":7,"method":method,"params":params})
                .to_string()
                .as_bytes(),
            backend,
        )
        .unwrap()
}
fn initialize() -> Value {
    json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{},
        "clientInfo":{"name":"test","version":"1"}})
}
fn ready(server: &mut Server, backend: &mut Fake) {
    let result = request(server, backend, "initialize", initialize());
    assert_eq!(result["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(result["result"]["capabilities"], json!({"tools":{}}));
    assert!(
        server
            .handle(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                backend
            )
            .is_none()
    );
}

#[test]
fn initialize_list_and_ping() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    assert_eq!(
        request(&mut server, &mut backend, "tools/list", json!({}))["error"]["code"],
        -32002
    );
    ready(&mut server, &mut backend);
    let listed = request(&mut server, &mut backend, "tools/list", json!({}));
    assert_eq!(listed["result"]["tools"], list_tools());
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 5);
    assert_eq!(
        request(&mut server, &mut backend, "ping", json!({}))["result"],
        json!({})
    );
}

#[test]
fn every_tool_forwards_arguments_and_backend_failures_are_tool_errors() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    let cases = [
        (
            "session_create",
            json!({"prompt":"test","directory":"/tmp","idempotency_key":"key"}),
        ),
        ("session_list", json!({})),
        ("session_get", json!({"session_id":"s"})),
        (
            "session_send",
            json!({"session_id":"s","message":"next","message_id":"m"}),
        ),
        ("session_cancel", json!({"session_id":"s","mode":"hard"})),
    ];
    for (name, args) in cases {
        let response = request(
            &mut server,
            &mut backend,
            "tools/call",
            json!({"name":name,"arguments":args}),
        );
        assert_eq!(backend.calls.last(), Some(&(name.to_string(), args)));
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["id"], 7);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "{\"id\":\"session-1\"}"
        );
    }
    backend.fail = true;
    let response = request(
        &mut server,
        &mut backend,
        "tools/call",
        json!({"name":"session_list"}),
    );
    assert!(response.get("error").is_none());
    assert_eq!(response["result"]["isError"], true);
    let failure: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(failure["code"], "unsupported");
}

#[test]
fn bad_methods_params_and_tools_never_dispatch() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    assert_eq!(
        request(&mut server, &mut backend, "resources/list", json!({}))["error"]["code"],
        -32601
    );
    for params in [
        json!(null),
        json!([]),
        json!({}),
        json!({"name":"missing"}),
        json!({"name":"session_create","arguments":{"prompt":"test"}}),
        json!({"name":"session_cancel","arguments":{"session_id":"s","mode":"other"}}),
        json!({"name":"session_get","arguments":{"session_id":1}}),
        json!({"name":"session_list","arguments":{"extra":true}}),
        json!({"name":"session_get","arguments":{"session_id":""}}),
        json!({"name":"session_list","unexpected":true}),
    ] {
        assert_eq!(
            request(&mut server, &mut backend, "tools/call", params)["error"]["code"],
            -32602
        );
    }
    assert!(backend.calls.is_empty());
}

#[test]
fn notifications_never_execute_tools_or_reply() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    for method in ["tools/call", "unknown", "ping", "notifications/initialized"] {
        let notification =
            json!({"jsonrpc":"2.0","method":method,"params":{"name":"session_list"}});
        assert!(
            server
                .handle(notification.to_string().as_bytes(), &mut backend)
                .is_none()
        );
    }
    assert!(backend.calls.is_empty());
}

#[test]
fn reject_invalid_json_ids_envelopes_and_version() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    assert_eq!(
        server.handle(b"{", &mut backend).unwrap()["error"]["code"],
        -32700
    );
    for id in [json!(null), json!(true), json!([]), json!({}), json!(1.5)] {
        let input = json!({"jsonrpc":"2.0","id":id,"method":"ping"});
        let reply = server
            .handle(input.to_string().as_bytes(), &mut backend)
            .unwrap();
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], -32600);
    }
    for input in [
        json!([]),
        json!({"jsonrpc":"1.0","id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1,"result":{}}),
    ] {
        assert_eq!(
            server
                .handle(input.to_string().as_bytes(), &mut backend)
                .unwrap()["error"]["code"],
            -32600
        );
    }
    let mut params = initialize();
    params["protocolVersion"] = json!("unsupported");
    assert_eq!(
        request(&mut server, &mut backend, "initialize", params)["error"]["code"],
        -32602
    );
    ready(&mut server, &mut backend);
    let input = br#"{"jsonrpc":"2.0","id":"request-a","method":"ping"}"#;
    assert_eq!(
        server.handle(input, &mut backend).unwrap()["id"],
        "request-a"
    );
}

#[test]
fn transport_bounds_and_newline_frames() {
    let mut backend = Fake::default();
    let mut output = Vec::new();
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"method\":\"unknown\"}\n";
    serve(&mut Cursor::new(input), &mut output, &mut backend).unwrap();
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["result"], json!({}));
    for input in [vec![b'x'; MAX_FRAME_BYTES + 10], b"{}".to_vec()] {
        output.clear();
        let mut reader = Cursor::new(input);
        assert!(serve(&mut reader, &mut output, &mut backend).is_err());
        assert!(reader.position() <= (MAX_FRAME_BYTES + 2) as u64);
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["error"]["code"], -32600);
    }
}

#[test]
fn initialization_validation_does_not_unlock_tools() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    assert_eq!(
        request(
            &mut server,
            &mut backend,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION, "capabilities": []
            })
        )["error"]["code"],
        -32602
    );
    request(&mut server, &mut backend, "initialize", initialize());
    assert!(
        server
            .handle(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized","params":[]}"#,
                &mut backend
            )
            .is_none()
    );
    assert_eq!(
        request(&mut server, &mut backend, "tools/list", json!({}))["error"]["code"],
        -32002
    );
    assert_eq!(
        request(&mut server, &mut backend, "initialize", initialize())["error"]["code"],
        -32600
    );
}

#[test]
fn oversized_output_is_a_protocol_error() {
    struct Large;
    impl Backend for Large {
        fn call(&mut self, _: &str, _: Value) -> Result<Value, McpError> {
            Ok(json!("x".repeat(MAX_FRAME_BYTES)))
        }
    }
    let input = format!(
        "{}\n{}\n{}\n",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":initialize()}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_list"}})
    );
    let mut output = Vec::new();
    serve(&mut Cursor::new(input), &mut output, &mut Large).unwrap();
    let frames: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["error"]["code"], -32603);
    assert_eq!(frames[1]["id"], 2);
}

#[test]
fn frame_boundary_and_parse_errors_preserve_framing() {
    let mut backend = Fake::default();
    let mut output = Vec::new();
    let ping = br#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
    let mut input = ping.to_vec();
    input.resize(MAX_FRAME_BYTES, b' ');
    input.push(b'\n');
    input.extend_from_slice(b"\xff\n{\n");
    input.extend_from_slice(ping);
    input.push(b'\n');
    serve(&mut Cursor::new(input), &mut output, &mut backend).unwrap();
    let text = String::from_utf8(output).unwrap();
    let responses: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0]["result"], json!({}));
    assert_eq!(responses[1]["error"]["code"], -32700);
    assert_eq!(responses[2]["error"]["code"], -32700);
    assert_eq!(responses[3]["id"], 3);
}

#[test]
fn advertised_prompt_length_matches_unicode_validation() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    let tools = list_tools();
    let limit = tools[0]["inputSchema"]["properties"]["prompt"]["maxLength"]
        .as_u64()
        .unwrap() as usize;
    for (size, accepted) in [(limit, true), (limit + 1, false)] {
        let response = request(
            &mut server,
            &mut backend,
            "tools/call",
            json!({
                "name":"session_create", "arguments":{
                    "prompt":"🦀".repeat(size), "directory":"/tmp"
                }
            }),
        );
        assert_eq!(response.get("result").is_some(), accepted);
    }
    assert_eq!(backend.calls.len(), 1);
}
