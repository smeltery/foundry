#![allow(dead_code)]
use foundry_connectors::{
    Credentials, HttpLimits, MCP_PROTOCOL_VERSION, McpHttpClient, NetworkPolicy, Secret, Server,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: String,
    pub delay: Duration,
    pub chunked: bool,
}
impl Reply {
    pub fn raw(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
            headers: String::new(),
            delay: Duration::ZERO,
            chunked: false,
        }
    }
    pub fn result(id: u64, result: Value) -> Self {
        Self::raw(
            200,
            &json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
        )
    }
    pub fn initialize() -> Self {
        let mut reply = Self::result(
            1,
            json!({"protocolVersion":MCP_PROTOCOL_VERSION,
            "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
        );
        reply.headers = "Mcp-Session-Id: fixture-session\r\n".into();
        reply
    }
    pub fn notification() -> Self {
        Self::raw(202, "")
    }
}

pub struct Seen {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

pub struct Mock {
    pub origin: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let end = loop {
                    let mut chunk = [0; 2048];
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                    assert!(bytes.len() < 128 * 1024);
                    if let Some(end) = bytes.windows(4).position(|item| item == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let text = std::str::from_utf8(&bytes[..end]).unwrap();
                let mut lines = text.split("\r\n");
                let path = lines
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let headers: BTreeMap<_, _> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect();
                let length = headers["content-length"].parse::<usize>().unwrap();
                while bytes.len() < end + length {
                    let mut chunk = [0; 2048];
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                }
                capture.lock().unwrap().push(Seen {
                    path,
                    headers,
                    body: serde_json::from_slice(&bytes[end..end + length]).unwrap(),
                });
                tokio::time::sleep(reply.delay).await;
                let framing = if reply.chunked {
                    "Transfer-Encoding: chunked\r\n".into()
                } else {
                    format!("Content-Length: {}\r\n", reply.body.len())
                };
                let header = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nConnection: close\r\n{}{}\r\n",
                    reply.status, reply.content_type, framing, reply.headers
                );
                if socket.write_all(header.as_bytes()).await.is_err() {
                    continue;
                }
                if reply.chunked {
                    for chunk in reply.body.chunks(7) {
                        if socket
                            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if socket.write_all(chunk).await.is_err() {
                            break;
                        }
                        if socket.write_all(b"\r\n").await.is_err() {
                            break;
                        }
                    }
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                } else {
                    let _ = socket.write_all(&reply.body).await;
                }
            }
        });
        Self { origin, seen, task }
    }

    pub fn client(&self, limits: HttpLimits) -> McpHttpClient {
        let server: Server =
            serde_json::from_value(json!({"id":"fixture","transport":"streamable-http",
            "endpoint":{"kind":"remote","url":format!("{}/mcp?key=a%2Bb&key=two",self.origin)},
            "auth":{"kind":"api_key","placeholder":"credential"},
            "headers":{"Authorization":"Bearer ${credential}","X-Test":"local"}}))
            .unwrap();
        let policy = NetworkPolicy::local_http(&self.origin).unwrap();
        let credentials = Credentials::ApiKey(Secret::new("synthetic-test-token".into()).unwrap());
        let prepared = server.prepare_with_policy(&credentials, &policy).unwrap();
        McpHttpClient::new(prepared, policy, limits).unwrap()
    }
}
