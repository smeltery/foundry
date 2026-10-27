use foundry_mcp::{Backend, McpError, PROTOCOL_VERSION, Server, serve};
use serde_json::{Value, json};
use std::io::Cursor;

#[derive(Default)]
struct Fake(Vec<Value>);
impl Backend for Fake {
    fn call(&mut self, _: &str, args: Value) -> Result<Value, McpError> {
        self.0.push(args);
        Ok(json!({"sessions":[]}))
    }
}
fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}
fn handle(server: &mut Server, backend: &mut Fake, request: Value) -> Option<Value> {
    server.handle(request.to_string().as_bytes(), backend)
}
fn initialize() -> Value {
    request(
        "initialize",
        json!({"protocolVersion":PROTOCOL_VERSION,
        "capabilities":{},"clientInfo":{"name":"test","version":"1"},
        "_meta":{"com.example/context":{"nested":[true,null]}}}),
    )
}
fn ready(server: &mut Server, backend: &mut Fake) {
    let result = handle(server, backend, initialize()).unwrap();
    assert_eq!(result["result"]["capabilities"], json!({"tools":{}}));
    assert!(
        handle(
            server,
            backend,
            json!({"jsonrpc":"2.0",
        "method":"notifications/initialized","params":{"_meta":{"com.example/context":[]}}})
        )
        .is_none()
    );
}

#[test]
fn valid_metadata_is_opaque_and_never_forwarded_to_backend() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    for token in [json!("progress"), json!(0), json!(-1), json!(u64::MAX)] {
        let meta = json!({"progressToken":token,"com.example/context":{"values":[null,false]},
            "io.modelcontextprotocol/extension":42});
        for method in ["ping", "tools/list"] {
            let response = handle(
                &mut server,
                &mut backend,
                request(method, json!({"_meta":meta})),
            )
            .unwrap();
            assert!(response.get("result").is_some(), "{response}");
        }
        let response = handle(
            &mut server,
            &mut backend,
            request(
                "tools/call",
                json!({"name":"session_list","arguments":{},"_meta":meta}),
            ),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
    }
    assert_eq!(backend.0, vec![json!({}); 4]);
}

#[test]
fn malformed_metadata_and_tokens_fail_before_dispatch() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    let mut invalid = vec![json!(null), json!([]), json!(true), json!(3), json!("meta")];
    for token in [json!(null), json!(true), json!(1.5), json!([]), json!({})] {
        invalid.push(json!({"progressToken":token}));
    }
    for meta in invalid {
        for method in ["ping", "tools/list", "tools/call"] {
            let mut params = json!({"_meta":meta});
            if method == "tools/call" {
                params["name"] = json!("session_list");
                params["arguments"] = json!({});
            }
            let response = handle(&mut server, &mut backend, request(method, params)).unwrap();
            assert_eq!(response["error"]["code"], -32602);
        }
    }
    assert!(backend.0.is_empty());
}

#[test]
fn metadata_does_not_relax_tool_arguments_or_other_parameters() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    ready(&mut server, &mut backend);
    for params in [
        json!({"name":"session_list","arguments":{"_meta":{}},"_meta":{}}),
        json!({"name":"session_get","arguments":{},"_meta":{}}),
        json!({"name":"session_list","arguments":{},"surprise":true,"_meta":{}}),
    ] {
        let response = handle(&mut server, &mut backend, request("tools/call", params)).unwrap();
        assert_eq!(response["error"]["code"], -32602);
    }
    assert!(backend.0.is_empty());
}

#[test]
fn invalid_metadata_cannot_advance_initialization() {
    let (mut server, mut backend) = (Server::default(), Fake::default());
    let mut invalid = initialize();
    invalid["params"]["_meta"] = json!([]);
    assert_eq!(
        handle(&mut server, &mut backend, invalid).unwrap()["error"]["code"],
        -32602
    );
    assert!(
        handle(&mut server, &mut backend, initialize())
            .unwrap()
            .get("result")
            .is_some()
    );
    assert!(
        handle(
            &mut server,
            &mut backend,
            json!({"jsonrpc":"2.0",
        "method":"notifications/initialized","params":{"_meta":null}})
        )
        .is_none()
    );
    assert_eq!(
        handle(&mut server, &mut backend, request("tools/list", json!({}))).unwrap()["error"]["code"],
        -32002
    );
}

#[test]
fn progress_request_emits_only_response_and_no_fabricated_notifications() {
    let messages = [
        initialize(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        request(
            "tools/call",
            json!({"name":"session_list","arguments":{},"_meta":{"progressToken":"p"}}),
        ),
    ];
    let input = messages
        .iter()
        .map(|message| format!("{message}\n"))
        .collect::<String>();
    let mut output = Vec::new();
    serve(&mut Cursor::new(input), &mut output, &mut Fake::default()).unwrap();
    let text = String::from_utf8(output).unwrap();
    let responses: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert!(
        responses
            .iter()
            .all(|response| response.get("method").is_none())
    );
}
