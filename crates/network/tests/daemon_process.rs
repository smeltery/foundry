#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::json;
use std::process::Stdio;
use support::*;
use tokio::process::Command;
#[tokio::test]
async fn actual_process_mail_ack_shutdown_and_restart_preserve_identity() {
    let state = State::new();
    let mut child = state.start().await;
    let mut sender = state.connect().await;
    let mut receiver = state.connect().await;
    let alice = sender.register("sender", &state.0).await;
    let bob = receiver.register("receiver", &state.0).await;
    let sent = sender.request(json!({"id":2,"op":"send","auth":alice,"message_id":"one","to":{"name":"receiver"},"body":"hello"})).await;
    assert_eq!(sent["result"]["status"], "ok");
    let inbox = receiver
        .request(json!({"id":3,"op":"inbox","auth":bob,"peek":true}))
        .await;
    let attempt = inbox["result"]["value"]["messages"][0]["attempt_id"]
        .as_i64()
        .unwrap();
    let wrong = sender
        .request(json!({"id":4,"op":"delivered","auth":alice,"attempt_id":attempt}))
        .await;
    assert_eq!(wrong["result"]["status"], "error");
    let ack = receiver
        .request(json!({"id":5,"op":"delivered","auth":bob,"attempt_id":attempt}))
        .await;
    assert_eq!(ack["result"]["status"], "ok");
    let shutdown = receiver
        .request(json!({"id":6,"op":"shutdown","auth":bob}))
        .await;
    assert_eq!(shutdown["result"]["status"], "ok");
    exited(&mut child).await;
    drop(sender);
    drop(receiver);
    assert!(!state.0.join("daemon.sock").exists());
    let mut child = state.start().await;
    let mut receiver = state.connect().await;
    let attach = receiver
        .request(json!({"id":7,"op":"attach","auth":bob}))
        .await;
    assert_eq!(attach["result"]["status"], "ok");
    let unread = receiver
        .request(json!({"id":8,"op":"unread","auth":bob}))
        .await;
    assert_eq!(unread["result"]["value"]["count"], 0);
    receiver
        .request(json!({"id":9,"op":"shutdown","auth":bob}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn unknown_operations_keep_correlation_and_second_daemon_is_refused() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let rejected = client
        .request(json!({"id":42,"op":"unknown_operation"}))
        .await;
    assert_eq!(rejected["result"]["status"], "error");
    let status = tokio::time::timeout(
        DEADLINE,
        Command::new(env!("CARGO_BIN_EXE_f5y-daemon"))
            .arg("--config")
            .arg(state.0.join("config.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!status.success());
    let auth = client.register("owner", &state.0).await;
    client
        .request(json!({"id":43,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn killed_process_retains_pending_mail_and_scoped_credentials() {
    let state = State::new();
    let mut child = state.start().await;
    let mut sender = state.connect().await;
    let mut receiver = state.connect().await;
    let alice = sender.register("sender", &state.0).await;
    let bob = receiver.register("receiver", &state.0).await;
    let sent = sender.request(json!({"id":2,"op":"send","auth":alice,"message_id":"pending","to":{"name":"receiver"},"body":"retained"})).await;
    assert_eq!(sent["result"]["status"], "ok");
    child.kill().await.unwrap();
    drop(sender);
    drop(receiver);
    let mut child = state.start().await;
    let mut receiver = state.connect().await;
    let attached = receiver
        .request(json!({"id":3,"op":"attach","auth":bob}))
        .await;
    assert_eq!(attached["result"]["status"], "ok");
    let unread = receiver
        .request(json!({"id":4,"op":"unread","auth":bob}))
        .await;
    assert_eq!(unread["result"]["value"]["count"], 1);
    let inbox = receiver
        .request(json!({"id":5,"op":"inbox","auth":bob}))
        .await;
    assert_eq!(
        inbox["result"]["value"]["messages"][0]["message_id"],
        "pending"
    );
    receiver
        .request(json!({"id":6,"op":"shutdown","auth":bob}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn termination_signal_drains_connections_and_releases_ownership() {
    let state = State::new();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("owner", &state.0).await;
    let id = child.id().unwrap();
    // Target only the child this test owns; no pointers are passed to kill.
    assert_eq!(
        unsafe { libc::kill(i32::try_from(id).unwrap(), libc::SIGTERM) },
        0
    );
    exited(&mut child).await;
    drop(client);
    assert!(!state.0.join("daemon.sock").exists());
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let attached = client
        .request(json!({"id":2,"op":"attach","auth":auth}))
        .await;
    assert_eq!(attached["result"]["status"], "ok");
    client
        .request(json!({"id":3,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
