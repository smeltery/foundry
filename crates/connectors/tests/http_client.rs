mod support;
use foundry_connectors::{HttpLimits, MCP_PROTOCOL_VERSION, McpError};
use serde_json::json;
use std::time::Duration;
use support::{Mock, Reply};

#[tokio::test]
async fn initialize_list_call_and_pagination_preserve_wire_contract() {
    let server = Mock::start(vec![Reply::initialize(), Reply::notification(),
        Reply::result(2, json!({"tools":[{"name":"echo","inputSchema":{"type":"object"},"description":"fixture"}],"nextCursor":"page-two"})),
        Reply::result(3, json!({"content":[{"type":"text","text":"hello"}],"structuredContent":{"echo":"hello"},"isError":false})),
        Reply::result(4, json!({"tools":[]}))]).await;
    let mut client = server.client(HttpLimits::default());
    assert_eq!(
        client.list_tools(None).await.unwrap_err(),
        McpError::NotInitialized
    );
    client.initialize().await.unwrap();
    assert_eq!(
        client.initialize().await.unwrap_err(),
        McpError::AlreadyInitialized
    );
    let page = client.list_tools(None).await.unwrap();
    assert_eq!(page.tools[0].name, "echo");
    let result = client
        .call_tool(
            "echo",
            json!({"message":"hello"}).as_object().unwrap().clone(),
        )
        .await
        .unwrap();
    assert_eq!(result.content[0]["text"], "hello");
    assert!(!result.is_error);
    assert!(
        client
            .list_tools(page.next_cursor.as_deref())
            .await
            .unwrap()
            .tools
            .is_empty()
    );
    assert!(!format!("{client:?} {page:?} {result:?}").contains("synthetic-test-token"));
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 5);
    assert_eq!(seen[0].body["method"], "initialize");
    assert_eq!(
        seen[0].body["params"]["protocolVersion"],
        MCP_PROTOCOL_VERSION
    );
    assert_eq!(
        seen[1].body,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    );
    assert_eq!(seen[4].body["params"]["cursor"], "page-two");
    for (index, request) in seen.iter().enumerate() {
        assert_eq!(request.path, "/mcp?key=a%2Bb&key=two");
        assert_eq!(
            request.headers["authorization"],
            "Bearer synthetic-test-token"
        );
        assert_eq!(
            request.headers["accept"],
            "application/json, text/event-stream"
        );
        if index > 0 {
            assert_eq!(request.headers["mcp-session-id"], "fixture-session");
            assert_eq!(
                request.headers["mcp-protocol-version"],
                MCP_PROTOCOL_VERSION
            );
        }
    }
}

#[tokio::test]
async fn chunked_sse_handles_comments_notifications_and_multiline_data() {
    let mut reply = Reply::raw(
        200,
        concat!(
            ": heartbeat\r\n\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\r\n\r\n",
            "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\r\n",
            "data: \"id\":2,\"result\":{\"tools\":[]}}\r\n\r\n"
        ),
    );
    reply.content_type = "text/event-stream";
    reply.chunked = true;
    let server = Mock::start(vec![Reply::initialize(), Reply::notification(), reply]).await;
    let mut client = server.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert!(client.list_tools(None).await.unwrap().tools.is_empty());
}

#[tokio::test]
async fn tool_error_result_is_not_a_transport_error() {
    let server = Mock::start(vec![
        Reply::initialize(),
        Reply::notification(),
        Reply::result(
            2,
            json!({"content":[{"type":"text","text":"input rejected"}],"isError":true}),
        ),
    ])
    .await;
    let mut client = server.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert!(
        client
            .call_tool("echo", Default::default())
            .await
            .unwrap()
            .is_error
    );
}

#[tokio::test]
async fn redirects_do_not_forward_credentials_or_issue_second_request() {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut redirect = Reply::raw(307, "");
    redirect.headers = format!(
        "Location: http://{}/capture\r\n",
        sink.local_addr().unwrap()
    );
    let server = Mock::start(vec![redirect]).await;
    assert_eq!(
        server
            .client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err(),
        McpError::Redirect
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), sink.accept())
            .await
            .is_err()
    );
    assert_eq!(server.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn response_and_request_budgets_fail_before_unbounded_reads_or_sends() {
    let mut oversized = Reply::initialize();
    oversized.body = vec![b'x'; 2048];
    for chunked in [false, true] {
        let mut reply = Reply::raw(200, std::str::from_utf8(&oversized.body).unwrap());
        reply.chunked = chunked;
        let server = Mock::start(vec![reply]).await;
        let limits = HttpLimits {
            max_response_bytes: 256,
            ..HttpLimits::default()
        };
        assert_eq!(
            server.client(limits).initialize().await.unwrap_err(),
            McpError::TooLarge
        );
    }
    let server = Mock::start(vec![Reply::initialize()]).await;
    let limits = HttpLimits {
        max_request_bytes: 1,
        ..HttpLimits::default()
    };
    assert_eq!(
        server.client(limits).initialize().await.unwrap_err(),
        McpError::TooLarge
    );
    assert!(server.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn whole_operation_timeout_bounds_delayed_headers() {
    let mut reply = Reply::initialize();
    reply.delay = Duration::from_millis(200);
    let server = Mock::start(vec![reply]).await;
    let limits = HttpLimits {
        timeout: Duration::from_millis(20),
        ..HttpLimits::default()
    };
    assert_eq!(
        server.client(limits).initialize().await.unwrap_err(),
        McpError::Timeout
    );
}

#[tokio::test]
async fn rpc_errors_version_mismatch_and_bad_ids_are_neutral() {
    for (reply, expected) in [
        (
            Reply::raw(
                200,
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"synthetic-test-token"}}"#,
            ),
            McpError::RemoteRpc(-32001),
        ),
        (Reply::result(9, json!({})), McpError::Protocol),
        (
            Reply::result(1, json!({"protocolVersion":"unknown"})),
            McpError::UnsupportedVersion,
        ),
        (
            Reply::raw(401, "synthetic-test-token"),
            McpError::HttpStatus(401),
        ),
    ] {
        let server = Mock::start(vec![reply]).await;
        let error = server
            .client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("synthetic-test-token"));
    }
}

#[tokio::test]
async fn session_expiration_is_not_retried_and_server_requests_are_explicit() {
    let server = Mock::start(vec![
        Reply::initialize(),
        Reply::notification(),
        Reply::raw(404, ""),
    ])
    .await;
    let mut client = server.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert_eq!(
        client.list_tools(None).await.unwrap_err(),
        McpError::SessionExpired
    );
    assert_eq!(server.seen.lock().unwrap().len(), 3);
    let mut reply = Reply::raw(
        200,
        "data: {\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n\n",
    );
    reply.content_type = "text/event-stream";
    let server = Mock::start(vec![reply]).await;
    assert_eq!(
        server
            .client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err(),
        McpError::UnsupportedServerMessage
    );
}

#[tokio::test]
async fn absent_tools_capability_prevents_tool_operations() {
    let reply = Reply::result(
        1,
        json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}),
    );
    let server = Mock::start(vec![reply, Reply::notification()]).await;
    let mut client = server.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert_eq!(
        client.list_tools(None).await.unwrap_err(),
        McpError::ToolsUnavailable
    );
    assert_eq!(server.seen.lock().unwrap().len(), 2);
}
