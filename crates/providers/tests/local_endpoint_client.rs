//! Endpoint calls against the deterministic loopback stub: every wire format,
//! every failure category, pinned resolution and redirect refusal.

#[path = "local_endpoint_stub.rs"]
mod stub;

use foundry_providers::catalog::EndpointFormat;
use foundry_providers::local_endpoint::{
    Completion, CompletionRequest, EndpointError, FailureCategory, LocalEndpoint, Message, OptIn,
    Role,
};
use std::time::{Duration, Instant};
use stub::{BODY_CANARY, Behavior, FixtureResolver, KEY_CANARY, Stub, descriptor, model, settings};

fn request() -> CompletionRequest {
    CompletionRequest {
        model: model("local-small"),
        system: Some("be brief".into()),
        messages: vec![Message {
            role: Role::User,
            content: "hello".into(),
        }],
        max_output_tokens: 64,
    }
}

fn client(format: Option<EndpointFormat>, base_url: &str) -> LocalEndpoint {
    LocalEndpoint::new(&descriptor(format), settings(base_url, &[OptIn::Loopback])).unwrap()
}

async fn call(endpoint: &LocalEndpoint) -> Result<Completion, EndpointError> {
    endpoint
        .complete(&FixtureResolver::new(&[]), &request())
        .await
}

fn failure(category: FailureCategory) -> EndpointError {
    EndpointError::Failure {
        provider_id: descriptor(None).provider_id,
        category,
    }
}

/// The error text names the category and provider only.
fn assert_redacted(error: &EndpointError, stub: &Stub) {
    let text = format!("{error} {error:?}");
    for secret in [
        KEY_CANARY,
        BODY_CANARY,
        "/v1",
        &stub.addr.port().to_string(),
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    if let EndpointError::Failure { category, .. } = error {
        let prefix = format!("local-a endpoint failure: {}", category.name());
        assert!(error.to_string().starts_with(&prefix), "{error}");
    }
}

#[tokio::test]
async fn every_format_round_trips_through_the_stub() {
    let cases = [
        (
            None,
            r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
            "/v1/chat/completions",
            "authorization",
            "Bearer fixture-key-canary",
        ),
        (
            Some(EndpointFormat::Response),
            r#"{"output":[{"type":"note"},{"type":"message","content":[{"type":"output_text","text":"h"},{"type":"output_text","text":"i"}]}],"usage":{"input_tokens":3,"output_tokens":1}}"#,
            "/v1/responses",
            "authorization",
            "Bearer fixture-key-canary",
        ),
        (
            Some(EndpointFormat::Message),
            r#"{"content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":3,"output_tokens":1}}"#,
            "/v1/messages",
            "x-api-key",
            "fixture-key-canary",
        ),
    ];
    for (format, reply, path, key_header, key_value) in cases {
        let stub = Stub::start(vec![Behavior::json(200, reply)]).await;
        let completion = call(&client(format, &stub.url("http", "/v1/")))
            .await
            .unwrap();
        assert_eq!(completion.text, "hi");
        assert_eq!(
            (completion.input_tokens, completion.output_tokens),
            (Some(3), Some(1))
        );

        let seen = stub.seen.lock().unwrap();
        let body = &seen[0].body;
        assert_eq!(seen[0].path, path);
        assert_eq!(seen[0].headers[key_header], key_value);
        assert_eq!(
            (body["model"].as_str(), body["stream"].as_bool()),
            (Some("local-small"), Some(false))
        );
        match format {
            None => {
                assert_eq!(body["messages"][0]["role"], "system");
                assert_eq!(body["messages"][1]["content"], "hello");
                assert_eq!(body["max_tokens"], 64);
            }
            Some(EndpointFormat::Response) => {
                assert_eq!(body["instructions"], "be brief");
                assert_eq!(body["input"][0]["role"], "user");
                assert_eq!(body["max_output_tokens"], 64);
            }
            Some(_) => {
                assert_eq!(body["system"], "be brief");
                assert_eq!(body["messages"][0]["role"], "user");
                assert_eq!(body["max_tokens"], 64);
            }
        }
    }
}

#[tokio::test]
async fn timeout_failure_hides_url_and_key() {
    let stub = Stub::start(vec![Behavior::Stall]).await;
    let mut settings = settings(&stub.url("http", "/v1"), &[OptIn::Loopback]);
    settings.timeout = Duration::from_millis(300);
    let endpoint = LocalEndpoint::new(&descriptor(None), settings).unwrap();
    let started = Instant::now();
    let error = call(&endpoint).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(error, failure(FailureCategory::Timeout));
    assert_eq!(error.to_string(), "local-a endpoint failure: timeout");
    assert_eq!(error.class(), "endpoint_failure");
    assert_redacted(&error, &stub);
}

#[tokio::test]
async fn http_failure_reports_status_without_body() {
    let stub = Stub::start(vec![Behavior::json(500, BODY_CANARY)]).await;
    let error = call(&client(None, &stub.url("http", "/v1")))
        .await
        .unwrap_err();
    assert_eq!(error, failure(FailureCategory::Http(500)));
    assert_eq!(error.to_string(), "local-a endpoint failure: http 500");
    assert_redacted(&error, &stub);
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let target = Stub::start(vec![Behavior::json(200, "{}")]).await;
    for location in [
        target.url("http", "/v1/chat/completions"),
        "http://10.0.0.5/v1".into(),
    ] {
        let stub = Stub::start(vec![Behavior::Reply {
            status: 307,
            content_type: "application/json",
            headers: format!("location: {location}\r\n"),
            body: BODY_CANARY.into(),
        }])
        .await;
        let error = call(&client(None, &stub.url("http", "/v1")))
            .await
            .unwrap_err();
        assert_eq!(error, failure(FailureCategory::Http(307)));
        assert_redacted(&error, &stub);
    }
    assert_eq!(target.connections(), 0);
}

#[tokio::test]
async fn connect_dns_and_tls_failures_are_categorized() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let refused = call(&client(None, &format!("http://127.0.0.1:{port}/v1"))).await;
    assert_eq!(refused.unwrap_err(), failure(FailureCategory::Connect));

    let named = client(None, "http://stub.example/v1");
    let error = named.complete(&FixtureResolver::new(&[]), &request()).await;
    assert_eq!(error.unwrap_err(), failure(FailureCategory::Dns));

    let stub = Stub::start(vec![Behavior::NotTls]).await;
    let error = call(&client(None, &stub.url("https", "/v1")))
        .await
        .unwrap_err();
    assert_eq!(error, failure(FailureCategory::Tls));
    assert_redacted(&error, &stub);
}

#[tokio::test]
async fn unusable_replies_are_protocol_failures() {
    let oversized = format!(r#"{{"pad":"{}"}}"#, "x".repeat(4 * 1024 * 1024));
    for behavior in [
        Behavior::json(200, BODY_CANARY),
        Behavior::json(200, r#"{"choices":[]}"#),
        Behavior::json(200, &oversized),
        Behavior::Reply {
            status: 200,
            content_type: "text/html",
            headers: String::new(),
            body: "{}".into(),
        },
        // Decompression is off, so an encoded reply is refused unread.
        Behavior::Reply {
            status: 200,
            content_type: "application/json",
            headers: "content-encoding: gzip\r\n".into(),
            body: r#"{"choices":[{"message":{"content":"hi"}}]}"#.into(),
        },
    ] {
        let stub = Stub::start(vec![behavior]).await;
        let error = call(&client(None, &stub.url("http", "/v1")))
            .await
            .unwrap_err();
        assert_eq!(error, failure(FailureCategory::Protocol));
        assert_redacted(&error, &stub);
    }
}

#[tokio::test]
async fn resolution_is_pinned_and_rebinding_is_refused() {
    let reply = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
    let stub = Stub::start(vec![Behavior::json(200, reply)]).await;
    let endpoint = client(
        None,
        &format!("http://stub.example:{}/v1", stub.addr.port()),
    );
    // The second answer rebinds the name to a class the endpoint did not opt into.
    let resolver = FixtureResolver::new(&[&["127.0.0.1"], &["10.0.0.5"]]);
    assert!(endpoint.complete(&resolver, &request()).await.is_ok());
    let rebound = endpoint.complete(&resolver, &request()).await;
    assert_eq!(rebound.unwrap_err(), EndpointError::NotPermitted);
    assert_eq!((resolver.lookups(), stub.connections()), (2, 1));

    let mixed = FixtureResolver::new(&[&["127.0.0.1", "203.0.113.10"]]);
    let refused = endpoint.complete(&mixed, &request()).await;
    assert_eq!(refused.unwrap_err(), EndpointError::NotPermitted);
    assert_eq!(stub.connections(), 1);
}

#[tokio::test]
async fn unlisted_models_and_empty_requests_never_connect() {
    let stub = Stub::start(vec![Behavior::json(200, "{}")]).await;
    let endpoint = client(None, &stub.url("http", "/v1"));
    let resolver = FixtureResolver::new(&[]);
    let mut unlisted = request();
    unlisted.model = model("other-model");
    let error = endpoint.complete(&resolver, &unlisted).await.unwrap_err();
    assert_eq!(
        (error.clone(), error.class()),
        (EndpointError::UnknownModel, "not_found")
    );
    let mut empty = request();
    empty.messages.clear();
    assert_eq!(
        endpoint
            .complete(&resolver, &empty)
            .await
            .unwrap_err()
            .class(),
        "invalid_request"
    );
    assert_eq!(stub.connections(), 0);
}
