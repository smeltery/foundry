use super::{
    DEADLINE,
    child::Node,
    client::Client,
    config::{Config, Root},
};
use iroh::EndpointAddr;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Invitation {
    pub ticket: String,
    pub endpoint: EndpointAddr,
    pub code: Option<String>,
    pub intent: Option<Value>,
    #[serde(skip)]
    pub code_endpoint: Option<EndpointAddr>,
}
pub fn seed(config: &mut Config, address: &EndpointAddr) {
    let selected = json!({"endpoint":address.id,
        "direct":address.ip_addrs().collect::<Vec<_>>(),"relay":address.relay_urls().next().map(ToString::to_string)});
    let entries = config.value["network"]["seeds"].as_array_mut().unwrap();
    entries.retain(|entry| entry["endpoint"] != selected["endpoint"]);
    entries.push(selected);
    config.save();
}
pub async fn form(client: &mut Client, auth: &Value) -> Value {
    assert!(client.status(auth).await["team"].is_null());
    client
        .control(auth, json!({"kind":"form","name":"private fixture team"}))
        .await;
    let status = client.status(auth).await;
    assert!(status["team"].as_str().is_some());
    status
}
pub async fn invite(client: &mut Client, auth: &Value, short: bool) -> Invitation {
    let value = client
        .control(
            auth,
            json!({"kind":"invite","hours":1,"own":false,"short":short}),
        )
        .await;
    assert_eq!(value["kind"], "invitation");
    let mut result: Invitation = match serde_json::from_value(value) {
        Ok(value) => value,
        Err(_) => panic!("invitation contract"),
    };
    assert!(!result.ticket.is_empty());
    if short {
        let intent = result.intent.as_ref().expect("short invitation intent");
        result.code_endpoint = Some(publication(client, auth, intent).await);
    }
    result
}
pub async fn join(client: &mut Client, auth: &Value, invitation: &Invitation) {
    client
        .control(
            auth,
            json!({"kind":"join","ticket":invitation.ticket,
        "address":invitation.endpoint,"founder":invitation.endpoint.id}),
        )
        .await;
    assert!(client.status(auth).await["team"].is_string());
}
pub async fn pair(root: &Root) -> (Node, Client, Value, Node, Client, Value, Invitation) {
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    let status = form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, false).await;
    let mut config = root.config("b", "blair");
    seed(&mut config, &invitation.endpoint);
    let b = Node::start(config).await;
    let (mut cb, ab) = b.client().await;
    join(&mut cb, &ab, &invitation).await;
    assert_eq!(cb.status(&ab).await["team"], status["team"]);
    wait_remote(&mut ca, &b.config, true).await;
    wait_remote(&mut cb, &a.config, true).await;
    (a, ca, aa, b, cb, ab, invitation)
}
pub async fn wait_remote(client: &mut Client, remote: &Config, online: bool) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let value = client
                .ok(json!({"op":"who","filter":remote.destination()}))
                .await;
            let entries = value["entries"].as_array().expect("directory entries");
            if entries.iter().any(|entry| {
                entry["conversation"]["person"] == remote.person
                    && entry["conversation"]["name"] == remote.name
                    && entry["online"] == online
            }) {
                assert!(
                    entries.iter().all(|entry| entry["repository"].is_null()),
                    "unshared repository leaked"
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("authenticated remote directory deadline");
}
pub async fn send(client: &mut Client, auth: &Value, remote: &Config, id: &str, body: &str) {
    let value = client
        .ok(json!({"op":"send","auth":auth,"message_id":id,
        "to":remote.destination(),"body":body}))
        .await;
    assert_eq!(value["kind"], "transport_queued");
    assert_eq!(value["message_id"], id);
}
pub async fn peek(client: &mut Client, auth: &Value, expected: &[(&str, &str)]) -> Value {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let value = client
                .ok(json!({"op":"inbox","auth":auth,"peek":true}))
                .await;
            let messages = value["messages"].as_array().unwrap();
            if messages.len() >= expected.len() {
                check_mail(&value, expected, "pending");
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("durable inbox arrival deadline")
}
pub fn check_mail(value: &Value, expected: &[(&str, &str)], state: &str) {
    let messages = value["messages"].as_array().expect("inbox messages");
    assert_eq!(
        messages.len(),
        expected.len(),
        "complete exact inbox required"
    );
    for (mail, (id, body)) in messages.iter().zip(expected) {
        assert_eq!(mail["message_id"], *id);
        assert_eq!(
            data_encoding::BASE64
                .decode(mail["body_base64"].as_str().unwrap().as_bytes())
                .unwrap(),
            body.as_bytes()
        );
        assert_eq!(mail["state"], state);
    }
}
pub async fn consume(client: &mut Client, auth: &Value, expected: &[(&str, &str)]) {
    let value = client.ok(json!({"op":"inbox","auth":auth})).await;
    check_mail(&value, expected, "consumed");
    let empty = client.ok(json!({"op":"inbox","auth":auth})).await;
    check_mail(&empty, &[], "consumed");
}

pub async fn publication(client: &mut Client, auth: &Value, intent: &Value) -> EndpointAddr {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let value = client
                .control(auth, json!({"kind":"code_publications"}))
                .await;
            assert_eq!(value["kind"], "code_publications");
            let entries = value["publications"]
                .as_array()
                .expect("publications array");
            if let Some(entry) = entries.iter().find(|entry| entry["intent"] == *intent) {
                return match serde_json::from_value(entry["endpoint"].clone()) {
                    Ok(value) => value,
                    Err(_) => panic!("published endpoint contract"),
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("authorized code publication deadline")
}
