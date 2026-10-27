use super::{HttpLimits, HttpTransport, McpError};
use crate::{Credentials, NetworkPolicy, Secret, Server};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, ServerConfig, ServerConnection, StreamOwned};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "synthetic-tls-test-token";

#[derive(Default)]
struct Observation {
    accepted: bool,
    decrypted: Vec<u8>,
    handler_calls: usize,
    tls_error: bool,
}

struct Fixture {
    origin: String,
    certificate: CertificateDer<'static>,
    task: thread::JoinHandle<Observation>,
}

impl Fixture {
    fn start(identity: &str) -> Self {
        let signing_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![identity.to_owned()]).unwrap();
        params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let certificate = params.self_signed(&signing_key).unwrap().der().clone();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.clone()],
                    PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
                )
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let task = thread::spawn(move || {
            let mut seen = Observation::default();
            let deadline = Instant::now() + Duration::from_secs(8);
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return seen;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("local fixture accept failed: {error}"),
                }
            };
            seen.accepted = true;
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let connection = ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = StreamOwned::new(connection, socket);
            let mut buffer = [0; 1024];
            while seen.decrypted.len() < 16 * 1024 {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => seen.decrypted.extend_from_slice(&buffer[..length]),
                    Err(_) => {
                        seen.tls_error = true;
                        break;
                    }
                }
                if seen
                    .decrypted
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .is_some_and(|end| seen.decrypted.len() >= end + 6)
                {
                    seen.handler_calls += 1;
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                    stream.conn.send_close_notify();
                    stream.flush().unwrap();
                    break;
                }
            }
            seen
        });
        Self {
            origin,
            certificate,
            task,
        }
    }

    fn transport(&self, trust_fixture: bool) -> HttpTransport {
        let policy = NetworkPolicy::local_https(&self.origin);
        let mut transport = HttpTransport::new(
            prepared(&self.origin, &policy),
            policy,
            HttpLimits {
                timeout: Duration::from_secs(5),
                ..HttpLimits::default()
            },
        )
        .unwrap();
        if trust_fixture {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            // Only the trust roots change; the platform still verifies the peer identity.
            let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
                vec![self.certificate.clone()],
                provider.clone(),
            )
            .unwrap();
            transport.tls_override = Some(
                ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(verifier))
                    .with_no_client_auth(),
            );
        }
        transport
    }
}

fn prepared(origin: &str, policy: &NetworkPolicy) -> crate::PreparedRequest {
    let server: Server = serde_json::from_value(serde_json::json!({
        "id":"tls-fixture", "transport":"streamable-http",
        "endpoint":{"kind":"remote","url":format!("{origin}/mcp")},
        "auth":{"kind":"api_key","placeholder":"credential"},
        "headers":{"Authorization":"Bearer ${credential}"}
    }))
    .unwrap();
    let credentials = Credentials::ApiKey(Secret::new(TOKEN.into()).unwrap());
    server.prepare_with_policy(&credentials, policy).unwrap()
}

async fn assert_rejected(identity: &str, trust_fixture: bool) {
    let fixture = Fixture::start(identity);
    let result = fixture
        .transport(trust_fixture)
        .post(b"{}".to_vec(), None, false, false)
        .await;
    assert!(matches!(result, Err(McpError::Network)));
    let seen = fixture.task.join().unwrap();
    assert!(seen.accepted, "test must reach the TLS listener");
    assert!(seen.tls_error, "TLS must reject the handshake");
    assert!(seen.decrypted.is_empty(), "no application bytes may arrive");
    assert_eq!(seen.handler_calls, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn untrusted_certificate_blocks_credentials_with_default_verifier() {
    assert_rejected("127.0.0.1", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_certificate_with_wrong_identity_blocks_credentials() {
    assert_rejected("different.example", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_matching_certificate_reaches_handler() {
    let fixture = Fixture::start("127.0.0.1");
    let response = fixture
        .transport(true)
        .post(b"{}".to_vec(), None, false, false)
        .await;
    let seen = fixture.task.join().unwrap();
    assert!(
        response.is_ok(),
        "error={:?}, accepted={}, tls_error={}, bytes={}, calls={}",
        response.err(),
        seen.accepted,
        seen.tls_error,
        seen.decrypted.len(),
        seen.handler_calls
    );
    assert!(seen.accepted);
    assert!(!seen.tls_error);
    assert_eq!(seen.handler_calls, 1);
    let expected = format!("authorization: Bearer {TOKEN}");
    assert!(
        seen.decrypted
            .windows(expected.len())
            .any(|part| part == expected.as_bytes())
    );
}

#[test]
fn production_policy_rejects_cleartext_before_opening_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let request = prepared(&origin, &NetworkPolicy::local_http(&origin).unwrap());
    assert!(matches!(
        HttpTransport::new(request, NetworkPolicy::default(), HttpLimits::default()),
        Err(McpError::Policy)
    ));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(NetworkPolicy::default().url("http://8.8.8.8/mcp").is_err());
    assert!(
        NetworkPolicy::default()
            .url("http://example.com/mcp")
            .is_err()
    );
}
