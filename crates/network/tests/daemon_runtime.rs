#![cfg(unix)]
#[path = "daemon_process_support/mod.rs"]
mod support;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use support::*;
fn enabled(state: &State, person: &str) {
    let value = json!({"state_directory":state.0,"socket_directory":state.0,
        "person":person,"machine":"desk","folders":[],"network":{"mode":"enabled",
        "profile":{"team_alpn":[114,116,116],"join_alpn":[114,116,106],"protocol":31,
        "signing_prefix":"runtime-test-signatures/","invitation_endpoint_context":"runtime-test-endpoints",
        "invitation_secret_context":"runtime-test-secrets","ticket_prefixes":["runtime31"]},
        "transport":{"bind_address":"127.0.0.1:0","direct_paths":true,"relays":[],"relay_root_certificates":[]},
        "seeds":[],"trust":{"mode":"unverified"},"codebook":null,"ticket_relay_compression":null,
        "expected_team":null,"startup":{"mode":"existing"}}});
    let path = state.0.join("config.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
async fn control(client: &mut Client, auth: &Value, command: Value) -> Value {
    let reply = client
        .request(json!({"id":20,"op":"network","auth":auth,"command":command}))
        .await;
    assert_eq!(reply["result"]["status"], "ok", "{reply}");
    reply["result"]["value"].clone()
}
async fn remote(client: &mut Client, person: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let reply = client
                .request(json!({"id":21,"op":"who","filter":{"person":person,"online_only":true}}))
                .await;
            if reply["result"]["value"]["entries"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn actual_two_process_ticket_join_bidirectional_mail_and_stable_directory() {
    let (a, b) = (State::new(), State::new());
    enabled(&a, "amber");
    enabled(&b, "birch");
    let (mut left, mut right) = (a.start().await, b.start().await);
    let (mut ac, mut bc) = (a.connect().await, b.connect().await);
    let aa = ac.register("terminal", &a.0).await;
    let ba = bc.register("terminal", &b.0).await;
    let empty = control(&mut ac, &aa, json!({"kind":"status"})).await;
    assert!(empty["status"]["team"].is_null());
    control(
        &mut ac,
        &aa,
        json!({"kind":"form","name":"original-runtime-fixture"}),
    )
    .await;
    let invite = control(&mut ac, &aa, json!({"kind":"invite","hours":1})).await;
    control(
        &mut bc,
        &ba,
        json!({"kind":"join","ticket":invite["ticket"],"address":invite["endpoint"]}),
    )
    .await;
    remote(&mut ac, "birch").await;
    remote(&mut bc, "amber").await;
    deliver(&mut ac, &aa, &mut bc, &ba, "birch", "forward").await;
    deliver(&mut bc, &ba, &mut ac, &aa, "amber", "reverse").await;
    // Multiple actor ticks must keep the same actual readable remote directory.
    tokio::time::sleep(Duration::from_secs(2)).await;
    remote(&mut ac, "birch").await;
    remote(&mut bc, "amber").await;
    ac.request(json!({"id":24,"op":"shutdown","auth":aa})).await;
    bc.request(json!({"id":24,"op":"shutdown","auth":ba})).await;
    exited(&mut left).await;
    exited(&mut right).await;
}

async fn deliver(
    sender: &mut Client,
    auth: &Value,
    recipient: &mut Client,
    recipient_auth: &Value,
    to: &str,
    id: &str,
) {
    let sent = sender
        .request(json!({"id":22,"op":"send","auth":auth,"message_id":id,
        "to":{"person":to,"machine":"desk","name":"terminal"},"body":"runtime delivery"}))
        .await;
    assert_eq!(sent["result"]["status"], "ok", "{sent}");
    let inbox = recipient
        .request(json!({"id":23,"op":"inbox","auth":recipient_auth,"wait_secs":10,"peek":true}))
        .await;
    assert_eq!(
        inbox["result"]["value"]["messages"][0]["message_id"], id,
        "{inbox}"
    );
}

#[tokio::test]
async fn contradictory_ticket_pin_rejects_before_contact_and_leaves_empty_store() {
    let state = State::new();
    enabled(&state, "copper");
    let path = state.0.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["network"]["expected_team"] = json!("independent-pin");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    let form = client
        .request(
            json!({"id":30,"op":"network","auth":auth,"command":{"kind":"form","name":"pinned"}}),
        )
        .await;
    assert_eq!(form["result"]["status"], "error");
    let daemon_config: foundry_network::daemon_config::DaemonConfig =
        serde_json::from_value(config).unwrap();
    let resolved = match daemon_config.network {
        foundry_network::daemon_config::NetworkMode::Enabled(n) => n.resolve(&state.0).unwrap(),
        _ => unreachable!(),
    };
    let ticket = foundry_network::ticket::Ticket {
        team: "01010101010101010101010101010101".into(),
        secret: "02020202020202020202020202020202".into(),
        name: "unrelated".into(),
        relay: None,
        hint_relay: None,
        hint_ips: vec!["127.0.0.1:9".parse().unwrap()],
        peers: vec![iroh::SecretKey::generate().public()],
    };
    let text = resolved.ticket_codec().unwrap().encode(&ticket).unwrap();
    let joined = tokio::time::timeout(
        Duration::from_secs(2),
        client.request(
            json!({"id":31,"op":"network","auth":auth,"command":{"kind":"join","ticket":text}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(joined["result"]["status"], "error");
    let status = control(&mut client, &auth, json!({"kind":"status"})).await;
    assert!(status["status"]["team"].is_null());
    client
        .request(json!({"id":32,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn wrong_code_endpoint_and_conflicting_code_pins_send_no_transport_bytes() {
    let state = State::new();
    enabled(&state, "copper");
    let path = state.0.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let words = b"amber\ncedar\n";
    fs::write(state.0.join("words.txt"), words).unwrap();
    fs::set_permissions(state.0.join("words.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    config["network"]["codebook"] = json!({"path":"words.txt","sha256":data_encoding::HEXLOWER.encode(ring::digest::digest(&ring::digest::SHA256,words).as_ref())});
    config["network"]["expected_team"] = json!("common-pin");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let daemon_config: foundry_network::daemon_config::DaemonConfig =
        serde_json::from_value(config).unwrap();
    let resolved = match daemon_config.network {
        foundry_network::daemon_config::NetworkMode::Enabled(n) => n.resolve(&state.0).unwrap(),
        _ => unreachable!(),
    };
    let unrelated = foundry_network::NetworkEndpoint::bind(
        resolved.endpoint_config(iroh::SecretKey::generate()),
    )
    .await
    .unwrap();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    for team in [Value::Null, json!("contradictory-pin")] {
        let reply=client.request(json!({"id":33,"op":"network","auth":auth,"command":{"kind":"join_code","code":"amber-cedar-amber-cedar","address":unrelated.address(),"team":team}})).await;
        assert_eq!(reply["result"]["status"], "error");
    }
    assert!(matches!(
        unrelated.accept(Duration::from_millis(200)).await,
        Err(foundry_network::NetworkError::TimedOut)
    ));
    assert!(
        control(&mut client, &auth, json!({"kind":"status"})).await["status"]["team"].is_null()
    );
    client
        .request(json!({"id":34,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
    unrelated.shutdown().await;
}

#[path = "daemon_runtime/codes.rs"]
mod codes;

#[tokio::test]
async fn invalid_formation_proof_never_creates_a_bare_team_on_failed_or_existing_restart() {
    let state = State::new();
    enabled(&state, "copper");
    let path = state.0.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["network"]["startup"] =
        json!({"mode":"form","name":"proof-required","proof":"invalid-private-proof"});
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    for _ in 0..2 {
        let output = tokio::time::timeout(
            DEADLINE,
            tokio::process::Command::new(env!("CARGO_BIN_EXE_f5y-daemon"))
                .arg("--config")
                .arg(&path)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("invalid-private-proof"));
    }
    config["network"]["startup"] = json!({"mode":"existing"});
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    assert!(
        control(&mut client, &auth, json!({"kind":"status"})).await["status"]["team"].is_null()
    );
    client
        .request(json!({"id":61,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn direct_enabled_startup_remains_usable_when_its_explicit_relay_is_down() {
    let state = State::new();
    enabled(&state, "copper");
    let path = state.0.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["network"]["transport"]["relays"] = json!(["https://127.0.0.1:9"]);
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut child = tokio::time::timeout(Duration::from_secs(3), state.start())
        .await
        .unwrap();
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    control(
        &mut client,
        &auth,
        json!({"kind":"form","name":"direct-still-available"}),
    )
    .await;
    let status = control(&mut client, &auth, json!({"kind":"status"})).await;
    let address: iroh::EndpointAddr =
        serde_json::from_value(status["status"]["endpoint"].clone()).unwrap();
    assert!(address.ip_addrs().next().is_some());
    client
        .request(json!({"id":81,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}

#[tokio::test]
async fn refused_incoming_handshake_keeps_the_network_owner_and_local_controls_alive() {
    let state = State::new();
    enabled(&state, "copper");
    let mut child = state.start().await;
    let mut client = state.connect().await;
    let auth = client.register("terminal", &state.0).await;
    let original = control(&mut client, &auth, json!({"kind":"status"})).await;
    let address: iroh::EndpointAddr =
        serde_json::from_value(original["status"]["endpoint"].clone()).unwrap();
    let config: foundry_network::daemon_config::DaemonConfig =
        serde_json::from_slice(&fs::read(state.0.join("config.json")).unwrap()).unwrap();
    let settings = match config.network {
        foundry_network::daemon_config::NetworkMode::Enabled(n) => n.resolve(&state.0).unwrap(),
        _ => unreachable!(),
    };
    let mut transport = settings.endpoint_config(iroh::SecretKey::generate());
    transport.profile.team_alpn = b"unmatched-team-fixture".to_vec();
    transport.profile.join_alpn = b"unmatched-join-fixture".to_vec();
    let outsider = foundry_network::NetworkEndpoint::bind(transport)
        .await
        .unwrap();
    assert!(
        outsider
            .connect(address, foundry_network::Channel::Team, DEADLINE)
            .await
            .is_err()
    );
    let current = control(&mut client, &auth, json!({"kind":"status"})).await;
    assert_eq!(
        current["status"]["endpoint"]["id"],
        original["status"]["endpoint"]["id"]
    );
    client
        .request(json!({"id":91,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
    outsider.shutdown().await;
}

#[path = "daemon_runtime/connections.rs"]
mod connections;
