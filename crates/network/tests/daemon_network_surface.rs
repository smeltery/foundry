//! Network-enabled daemon process: shared status text and the bound UDP port.
#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio};
use support::*;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

const PREFIX: &str = "surface7";

fn enable(state: &State, bind_address: &str) {
    let value = json!({"state_directory":state.0,"socket_directory":state.0,
        "person":"quinn","machine":"desk","folders":[],"network":{"mode":"enabled",
        "profile":{"team_alpn":[115,102,116],"join_alpn":[115,102,106],"protocol":7,
        "signing_prefix":"surface-test-signatures/","invitation_endpoint_context":"surface-test-endpoints",
        "invitation_secret_context":"surface-test-secrets","ticket_prefixes":[PREFIX]},
        "transport":{"bind_address":bind_address,"direct_paths":true,"relays":[],"relay_root_certificates":[]},
        "seeds":[],"trust":{"mode":"unverified"},"codebook":null,"ticket_relay_compression":null,
        "expected_team":null,"startup":{"mode":"existing"}}});
    let path = state.0.join("config.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
async fn control(client: &mut Client, auth: &Value, command: Value) -> Value {
    let reply = client
        .request(json!({"id":40,"op":"network","auth":auth,"command":command}))
        .await;
    assert_eq!(reply["result"]["status"], "ok", "{reply}");
    reply["result"]["value"].clone()
}
async fn endpoint(client: &mut Client, auth: &Value) -> String {
    control(client, auth, json!({"kind":"status"})).await["status"]["endpoint"].to_string()
}

#[tokio::test]
async fn an_invitation_in_doing_is_shared_only_as_a_team_link() {
    let state = State::new();
    enable(&state, "127.0.0.1:0");
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    control(
        &mut client,
        &auth,
        json!({"kind":"form","name":"surface fixture"}),
    )
    .await;
    let invitation = control(&mut client, &auth, json!({"kind":"invite","hours":1})).await;
    let ticket = invitation["ticket"].as_str().unwrap().to_owned();
    assert!(ticket.starts_with(PREFIX));
    let doing = format!("pairing, join with {ticket} today");
    let set = client
        .request(json!({"id":41,"op":"set_status","auth":auth,"status":ticket,"doing":doing}))
        .await;
    assert_eq!(set["result"]["status"], "ok", "{set}");
    let who = client.request(json!({"id":42,"op":"who"})).await;
    let entry = &who["result"]["value"]["entries"][0];
    assert_eq!(entry["doing"], "pairing, join with (team link) today");
    assert_eq!(entry["status"], "(team link)");
    let secret = &ticket[PREFIX.len()..];
    let status = control(&mut client, &auth, json!({"kind":"status"})).await;
    for output in [who, status] {
        assert!(!output.to_string().contains(secret), "{output}");
    }
    client
        .request(json!({"id":43,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

/// Start the daemon, or return `None` if it exits before reporting readiness.
async fn try_start(state: &State) -> Option<Child> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_f5y-daemon"))
        .arg("--config")
        .arg(state.0.join("config.json"))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut line = String::new();
    let stdout = child.stdout.take().unwrap();
    tokio::time::timeout(DEADLINE, BufReader::new(stdout).read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    if line.is_empty() {
        assert!(!child.wait().await.unwrap().success());
        return None;
    }
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready");
    Some(child)
}
/// Bind to a port found free a moment earlier. Another process can take it in
/// between, so a failed start is retried with a fresh port.
async fn start_on_free_port(state: &State) -> (Child, u16) {
    for _ in 0..5 {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        enable(state, &format!("127.0.0.1:{port}"));
        if let Some(child) = try_start(state).await {
            return (child, port);
        }
    }
    panic!("no free configured port could be bound in five attempts");
}
async fn shut_down(mut child: Child, client: &mut Client, auth: &Value) {
    client
        .request(json!({"id":44,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn a_free_configured_port_is_used_and_port_zero_picks_one() {
    let state = State::new();
    let (child, port) = start_on_free_port(&state).await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    let address = endpoint(&mut client, &auth).await;
    assert!(address.contains(&format!("127.0.0.1:{port}")), "{address}");
    shut_down(child, &mut client, &auth).await;

    let state = State::new();
    enable(&state, "127.0.0.1:0");
    let child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    let address = endpoint(&mut client, &auth).await;
    assert!(address.contains("127.0.0.1:"), "{address}");
    assert!(!address.contains("127.0.0.1:0\""), "{address}");
    shut_down(child, &mut client, &auth).await;
}

#[tokio::test]
async fn a_taken_configured_port_stops_startup_instead_of_moving() {
    let holder = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = holder.local_addr().unwrap().port();
    let state = State::new();
    enable(&state, &format!("127.0.0.1:{port}"));
    let output = tokio::time::timeout(
        DEADLINE,
        Command::new(env!("CARGO_BIN_EXE_f5y-daemon"))
            .arg("--config")
            .arg(state.0.join("config.json"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    // The host reports a fixed startup failure without echoing the address.
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: daemon startup failed\n"
    );
    assert!(output.stdout.is_empty());
    assert!(!state.0.join("daemon.sock").exists());
    // The held port was the only cause: once released, the same config binds it.
    drop(holder);
    let mut started = None;
    for _ in 0..3 {
        started = try_start(&state).await;
        if started.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let child = started.expect("released port could not be bound");
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    let address = endpoint(&mut client, &auth).await;
    assert!(address.contains(&format!("127.0.0.1:{port}")), "{address}");
    shut_down(child, &mut client, &auth).await;
}
