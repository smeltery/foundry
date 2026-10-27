mod support;
use foundry_connectors::{Credentials, HttpLimits, McpError, McpHttpClient, NetworkPolicy, Server};
use serde_json::json;
use support::{Mock, Reply};

fn server(endpoint: &str) -> Server {
    serde_json::from_value(json!({"id":"fixture","transport":"streamable-http",
        "endpoint":{"kind":"remote","url":endpoint},"auth":{"kind":"none"}}))
    .unwrap()
}

#[test]
fn explicit_loopback_policy_cannot_authorize_other_ports_or_hosts() {
    let policy = NetworkPolicy::local_http("http://127.0.0.1:43210").unwrap();
    for endpoint in [
        "http://127.0.0.1:43211/mcp",
        "http://127.0.0.2:43210/mcp",
        "http://10.0.0.1:43210/mcp",
        "https://api.example/mcp",
    ] {
        assert!(
            server(endpoint)
                .prepare_with_policy(&Credentials::None, &policy)
                .is_err()
        );
    }
    let endpoint = server("http://127.0.0.1:43210/mcp");
    assert!(endpoint.prepare(&Credentials::None).is_err());
    let prepared = endpoint
        .prepare_with_policy(&Credentials::None, &policy)
        .unwrap();
    assert_eq!(
        McpHttpClient::new(prepared, NetworkPolicy::default(), HttpLimits::default()).unwrap_err(),
        McpError::Policy
    );
    for origin in [
        "http://localhost:43210",
        "http://10.0.0.1:43210",
        "http://127.0.0.1:0",
        "http://127.0.0.1:43210/path",
        "http://127.0.0.1:43210?query",
        "https://127.0.0.1:43210",
    ] {
        assert!(NetworkPolicy::local_http(origin).is_err());
    }
}

#[tokio::test]
async fn duplicate_session_headers_and_nonempty_notification_fail() {
    let mut reply = Reply::initialize();
    reply.headers.push_str("Mcp-Session-Id: second-session\r\n");
    let mock = Mock::start(vec![reply]).await;
    assert_eq!(
        mock.client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err(),
        McpError::Protocol
    );
    let mock = Mock::start(vec![Reply::initialize(), Reply::raw(202, "nonempty")]).await;
    assert_eq!(
        mock.client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err(),
        McpError::Protocol
    );
}

#[tokio::test]
async fn changed_session_and_unknown_content_types_fail() {
    let mut changed = Reply::result(2, json!({"tools":[]}));
    changed.headers = "Mcp-Session-Id: different-session\r\n".into();
    let mock = Mock::start(vec![Reply::initialize(), Reply::notification(), changed]).await;
    let mut client = mock.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert_eq!(
        client.list_tools(None).await.unwrap_err(),
        McpError::Protocol
    );
    let mut reply = Reply::initialize();
    reply.content_type = "text/plain";
    let mock = Mock::start(vec![reply]).await;
    assert_eq!(
        mock.client(HttpLimits::default())
            .initialize()
            .await
            .unwrap_err(),
        McpError::UnsupportedContentType
    );
}

#[tokio::test]
async fn sse_cr_only_delimiter_is_processed_at_eof() {
    let mut reply = Reply::raw(
        200,
        "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\r\r",
    );
    reply.content_type = "text/event-stream";
    let mock = Mock::start(vec![Reply::initialize(), Reply::notification(), reply]).await;
    let mut client = mock.client(HttpLimits::default());
    client.initialize().await.unwrap();
    assert!(client.list_tools(None).await.unwrap().tools.is_empty());
}

#[test]
fn proxy_environment_is_ignored() {
    if std::env::var_os("FOUNDRY_PROXY_TEST_CHILD").is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mock = Mock::start(vec![Reply::initialize(), Reply::notification()]).await;
                mock.client(HttpLimits::default())
                    .initialize()
                    .await
                    .unwrap();
            });
        return;
    }
    let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}", trap.local_addr().unwrap());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "proxy_environment_is_ignored", "--nocapture"])
        .env("FOUNDRY_PROXY_TEST_CHILD", "1")
        .env("NO_PROXY", "")
        .env("no_proxy", "");
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        child.env(key, &proxy);
    }
    let result = child.output().unwrap();
    assert!(result.status.success(), "proxy-isolation child failed");
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
