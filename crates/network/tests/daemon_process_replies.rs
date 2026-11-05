//! Reply correlation and machine-readable error codes from the daemon process.
#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::{Value, json};
use support::*;

const CODES: [&str; 17] = [
    "invalid_input",
    "invalid_payload",
    "unsupported_payload",
    "unauthorized",
    "excluded",
    "not_found",
    "ambiguous",
    "limit",
    "conflict",
    "stale_attempt",
    "invalid_transition",
    "storage_uncertain",
    "schema",
    "clock",
    "entropy",
    "async_required",
    "output",
];
fn error_code(reply: &Value) -> &str {
    assert_eq!(reply["result"]["status"], "error", "{reply}");
    let code = reply["result"]["value"].as_str().unwrap();
    assert!(CODES.contains(&code), "{code}");
    code
}

#[tokio::test]
async fn requests_with_invalid_arguments_still_carry_their_id() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("owner", &state.0).await;
    let malformed = [
        json!({"id":31,"op":"send","auth":auth}),
        json!({"id":32,"op":"register","name":5,"repository":state.0}),
        json!({"id":33,"op":"who","filter":{"surprise":true}}),
        json!({"id":34,"op":"set_status","auth":auth,"status":["ready"]}),
        json!({"id":35,"op":"inbox","auth":auth,"wait_secs":-1}),
        json!({"id":36,"op":"delivered","auth":auth,"attempt_id":"one"}),
    ];
    for request in malformed {
        // `request` asserts the reply id equals the request id.
        let reply = client.request(request).await;
        assert_eq!(error_code(&reply), "invalid_input", "{reply}");
    }
    let network = client
        .request(json!({"id":37,"op":"network","auth":auth,"command":{"kind":"unknown"}}))
        .await;
    assert_eq!(network["result"]["status"], "error", "{network}");
    let disabled = client
        .request(json!({"id":38,"op":"network","auth":auth,"command":{"kind":"status"}}))
        .await;
    assert_eq!(disabled["result"]["status"], "error", "{disabled}");
    assert_ne!(network["result"]["value"], disabled["result"]["value"]);
    client
        .request(json!({"id":39,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn failures_carry_a_stable_code_and_successes_carry_none() {
    let state = State::new();
    let mut child = state.start().await;
    let mut owner = state.connect().await;
    let mut other = state.connect().await;
    let auth = owner.register("owner", &state.0).await;
    let taken = other
        .request(json!({"id":2,"op":"register","name":"owner","repository":state.0}))
        .await;
    assert_eq!(error_code(&taken), "unauthorized");
    let missing = owner
        .request(json!({"id":3,"op":"send","auth":auth,"message_id":"m","to":{"name":"nobody"},"body":"x"}))
        .await;
    assert_eq!(error_code(&missing), "not_found");
    let foreign = other
        .request(json!({"id":4,"op":"unread","auth":auth}))
        .await;
    assert_eq!(error_code(&foreign), "unauthorized");
    let missing_path = state.0.join("absent");
    let absent = other
        .request(json!({"id":5,"op":"register","name":"elsewhere","repository":missing_path}))
        .await;
    assert_eq!(error_code(&absent), "invalid_input");
    for request in [
        json!({"id":6,"op":"unread","auth":auth}),
        json!({"id":7,"op":"set_status","auth":auth,"status":"ready"}),
        json!({"id":8,"op":"who"}),
    ] {
        let reply = owner.request(request).await;
        assert_eq!(reply["result"]["status"], "ok", "{reply}");
        assert!(reply["result"]["value"].is_object(), "{reply}");
        assert!(reply["result"]["value"].get("error").is_none());
        assert!(reply.get("error").is_none());
    }
    owner
        .request(json!({"id":9,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
