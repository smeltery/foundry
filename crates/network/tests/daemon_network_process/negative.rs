use super::{
    child::{Node, fails_to_start},
    client::Client,
    config::{Root, profile},
    flow::*,
};
use foundry_network::{EndpointConfig, NetworkEndpoint, RelaySelection, WireProfile};
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_join_rejects_wrong_endpoint_before_any_bearer_request_reaches_it() {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, true).await;
    let code = invitation.code.as_ref().unwrap();
    let wrong = NetworkEndpoint::bind(EndpointConfig {
        profile: serde_json::from_value::<WireProfile>(profile()).unwrap(),
        secret_key: iroh::SecretKey::generate(),
        relay: RelaySelection::Disabled,
        bind_address: "127.0.0.1:0".parse().unwrap(),
        direct_paths: true,
        relay_root_certificates: vec![],
    })
    .await
    .unwrap();
    assert!(wrong.id() != invitation.code_endpoint.as_ref().unwrap().id);
    let b = Node::start(root.config("b", "blair")).await;
    let (mut cb, ab) = b.client().await;
    let observe = async {
        match wrong.accept(Duration::from_secs(2)).await {
            Ok(connection) => {
                let streams =
                    tokio::time::timeout(Duration::from_secs(2), connection.accept_bi()).await;
                if let Ok(Ok((_, mut input))) = streams {
                    // This is a hostile owned fixture, not an application peer.
                    // Drain only a finite request; never format its bearer bytes.
                    let _request =
                        tokio::time::timeout(Duration::from_secs(2), input.read_to_end(65536))
                            .await;
                    connection.close(0u32.into(), b"fixture complete");
                    return true;
                }
                connection.close(0u32.into(), b"fixture complete");
                true
            }
            Err(_) => false,
        }
    };
    let request = cb.request(json!({"op":"network","auth":ab,
        "command":{"kind":"join_code","code":code,"address":wrong.address()}}));
    let (exposed, reply) = tokio::join!(observe, request);
    assert!(
        !exposed,
        "wrong code endpoint was contacted before the derived identity gate"
    );
    assert_eq!(reply["result"]["status"], "error");
    assert!(cb.status(&ab).await["team"].is_null());
    wrong.shutdown().await;
    b.stop(&mut cb, &ab, &[code]).await;
    a.stop(&mut ca, &aa, &[code]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn common_team_pin_rejects_both_join_forms_without_committing_a_joined_store() {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    for (label, short) in [("b", false), ("c", true)] {
        let invitation = invite(&mut ca, &aa, short).await;
        let mut cfg = root.config(label, if short { "casey" } else { "blair" });
        cfg.value["network"]["expected_team"] = json!("independent-mismatching-team");
        let b = Node::start(cfg).await;
        let (mut cb, ab) = b.client().await;
        let command = if short {
            json!({"kind":"join_code","code":invitation.code,
                "address":invitation.code_endpoint})
        } else {
            json!({"kind":"join","ticket":invitation.ticket,"address":invitation.endpoint})
        };
        let reply = cb
            .request(json!({"op":"network","auth":ab,"command":command}))
            .await;
        assert_eq!(reply["result"]["status"], "error");
        assert!(
            cb.status(&ab).await["team"].is_null(),
            "mismatching pin committed a local team"
        );
        let cfg = b.config.clone();
        b.stop(&mut cb, &ab, &[]).await;
        let b = Node::start(cfg).await;
        let mut cb = Client::connect(&b.config).await;
        cb.attach(&ab).await;
        assert!(
            cb.status(&ab).await["team"].is_null(),
            "mismatching pin survived as a joined checkpoint"
        );
        b.stop(&mut cb, &ab, &[]).await;
    }
    a.stop(&mut ca, &aa, &[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_configured_formation_proof_cannot_reopen_as_successful_bare_membership() {
    let root = Root::new();
    let mut config = root.config("a", "alex");
    config.value["network"]["startup"] = json!({"mode":"form","name":"must not silently form",
        "proof":"invalid-private-proof-marker"});
    fails_to_start(&config).await;
    fails_to_start(&config).await;
    config.value["network"]["startup"] = json!({"mode":"existing"});
    let node = Node::start(config).await;
    let (mut client, auth) = node.client().await;
    assert!(client.status(&auth).await["team"].is_null());
    node.stop(&mut client, &auth, &["invalid-private-proof-marker"])
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consumed_long_startup_invitation_reopens_active_without_redemption() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, invitation) = pair(&root).await;
    let team = cb.status(&ab).await["team"].clone();
    let mut config = b.config.clone();
    b.stop(&mut cb, &ab, &[]).await;
    config.value["network"]["expected_team"] = team.clone();
    config.value["network"]["startup"] = json!({"mode":"join_ticket","ticket":invitation.ticket});
    let b = Node::start(config).await;
    let mut cb = Client::connect(&b.config).await;
    cb.attach(&ab).await;
    assert_eq!(cb.status(&ab).await["team"], team);
    b.stop(&mut cb, &ab, &[&invitation.ticket]).await;
    a.stop(&mut ca, &aa, &[&invitation.ticket]).await;
}
