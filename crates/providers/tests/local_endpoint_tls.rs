//! HTTPS against a loopback stub with a freshly generated self-signed
//! certificate: the platform verifier rejects it, and an explicitly trusted
//! root accepts it.

#[path = "local_endpoint_stub.rs"]
mod stub;

use foundry_providers::local_endpoint::{
    CompletionRequest, EndpointError, FailureCategory, LocalEndpoint, Message, OptIn, Role,
};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use stub::{FixtureResolver, KEY_CANARY, descriptor, model, settings};

const REPLY: &str = r#"{"choices":[{"message":{"content":"hi"}}]}"#;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Serves one HTTPS exchange. The thread returns whether a request arrived.
fn start_tls_stub() -> (String, CertificateDer<'static>, thread::JoinHandle<bool>) {
    let signing_key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let certificate = params.self_signed(&signing_key).unwrap().der().clone();
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("https://{}/v1", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let connection = ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = StreamOwned::new(connection, socket);
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        // The request is small; its body follows the headers in the same flight.
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(read) if read > 0 => request.extend_from_slice(&buffer[..read]),
                _ => return false,
            }
        }
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{REPLY}",
            REPLY.len()
        );
        stream.write_all(reply.as_bytes()).is_ok()
    });
    (url, certificate, task)
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: model("local-small"),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "hello".into(),
        }],
        max_output_tokens: 8,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn self_signed_certificate_is_rejected_by_the_verifier() {
    let (url, _, task) = start_tls_stub();
    let endpoint =
        LocalEndpoint::new(&descriptor(None), settings(&url, &[OptIn::Loopback])).unwrap();
    let error = endpoint
        .complete(&FixtureResolver::new(&[]), &request())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "local-a endpoint failure: tls");
    assert!(matches!(
        error,
        EndpointError::Failure {
            category: FailureCategory::Tls,
            ..
        }
    ));
    let text = format!("{error} {error:?}");
    assert!(
        !text.contains(KEY_CANARY) && !text.contains("/v1"),
        "{text}"
    );
    assert!(
        !task.join().unwrap(),
        "no request crosses a rejected handshake"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn explicitly_trusted_root_completes_over_https() {
    let (url, certificate, task) = start_tls_stub();
    let mut roots = RootCertStore::empty();
    roots.add(certificate).unwrap();
    let tls = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let endpoint = LocalEndpoint::new(&descriptor(None), settings(&url, &[OptIn::Loopback]))
        .unwrap()
        .with_tls_config(tls);
    let completion = endpoint
        .complete(&FixtureResolver::new(&[]), &request())
        .await
        .unwrap();
    assert_eq!(completion.text, "hi");
    assert!(task.join().unwrap());
}
