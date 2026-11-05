#![allow(dead_code)]
//! Deterministic stub endpoint and fixture resolver for the local endpoint
//! tests. It binds loopback only, replies from a fixed script and never calls
//! a real service.

use foundry_providers::catalog::{
    AuthKind, EndpointFormat, LoginUnavailable, ModelId, OauthLogin, ProviderDescriptor,
    ProviderId, ProviderKind,
};
use foundry_providers::local_endpoint::{EndpointKey, EndpointSettings, OptIn, Resolve};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const KEY_CANARY: &str = "fixture-key-canary";
pub const BODY_CANARY: &str = "fixture-body-canary";

pub fn descriptor(format: Option<EndpointFormat>) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: ProviderId::new("local-a").unwrap(),
        kind: ProviderKind::LocalEndpoint,
        auth_kinds: vec![AuthKind::None, AuthKind::ApiKey],
        config_fields: vec![],
        oauth_login: OauthLogin::Unavailable(LoginUnavailable::NotSupported),
        efforts: vec![],
        endpoint_format: format,
    }
}

pub fn model(value: &str) -> ModelId {
    ModelId::new(value).unwrap()
}

pub fn settings(base_url: &str, opt_in: &[OptIn]) -> EndpointSettings {
    EndpointSettings {
        base_url: base_url.into(),
        models: vec![model("local-small")],
        opt_in: opt_in.to_vec(),
        timeout: Duration::from_secs(5),
        key: Some(EndpointKey::new(KEY_CANARY.into()).unwrap()),
    }
}

/// Answers lookups from a script: each lookup takes the next answer and the
/// last one repeats. An empty script fails every lookup.
pub struct FixtureResolver {
    answers: Vec<Vec<IpAddr>>,
    pub lookups: AtomicUsize,
}

impl FixtureResolver {
    pub fn new(answers: &[&[&str]]) -> Self {
        Self {
            answers: answers
                .iter()
                .map(|set| set.iter().map(|ip| ip.parse().unwrap()).collect())
                .collect(),
            lookups: AtomicUsize::new(0),
        }
    }

    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl Resolve for FixtureResolver {
    async fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        let index = self.lookups.fetch_add(1, Ordering::SeqCst);
        self.answers
            .get(index)
            .or(self.answers.last())
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fixture lookup failure"))
    }
}

#[derive(Clone)]
pub enum Behavior {
    Reply {
        status: u16,
        content_type: &'static str,
        headers: String,
        body: String,
    },
    /// Reads the request, then holds the connection open without replying.
    Stall,
    /// Writes bytes that are not a TLS handshake, then closes.
    NotTls,
}

impl Behavior {
    pub fn json(status: u16, body: &str) -> Self {
        Self::Reply {
            status,
            content_type: "application/json",
            headers: String::new(),
            body: body.into(),
        }
    }
}

pub struct Seen {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

pub struct Stub {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    connections: Arc<AtomicUsize>,
}

impl Stub {
    /// Serves one behavior per connection in order; the last one repeats.
    pub async fn start(script: Vec<Behavior>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (task_seen, task_connections) = (seen.clone(), connections.clone());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let index = task_connections.fetch_add(1, Ordering::SeqCst);
                let behavior = script[index.min(script.len() - 1)].clone();
                tokio::spawn(serve(stream, behavior, task_seen.clone()));
            }
        });
        Self {
            addr,
            seen,
            connections,
        }
    }

    pub fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://{}{path}", self.addr)
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

async fn serve(mut stream: TcpStream, behavior: Behavior, seen: Arc<Mutex<Vec<Seen>>>) {
    if let Behavior::NotTls = behavior {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return;
    }
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    seen.lock().unwrap().push(request);
    match behavior {
        Behavior::Reply {
            status,
            content_type,
            headers,
            body,
        } => {
            let reply = format!(
                "HTTP/1.1 {status} Fixture\r\ncontent-type: {content_type}\r\n{headers}\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
        Behavior::Stall => tokio::time::sleep(Duration::from_secs(30)).await,
        Behavior::NotTls => unreachable!(),
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let head_end = loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut buffer).await.ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8(data[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let path = lines.next()?.split(' ').nth(1)?.to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers.get("content-length")?.parse().ok()?;
    while data.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buffer[..read]);
    }
    let body = serde_json::from_slice(&data[head_end..head_end + length]).ok()?;
    Some(Seen {
        path,
        headers,
        body,
    })
}
