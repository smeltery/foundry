use super::*;
fn codes(state: &State) {
    let path = state.0.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let words = b"amber\ncedar\n";
    fs::write(state.0.join("words.txt"), words).unwrap();
    fs::set_permissions(state.0.join("words.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    config["network"]["codebook"] = json!({"path":"words.txt","sha256":data_encoding::HEXLOWER.encode(ring::digest::digest(&ring::digest::SHA256,words).as_ref())});
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}
async fn publication(client: &mut Client, auth: &Value, nonempty: bool) -> Value {
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let reply=client.request(json!({"id":51,"op":"network","auth":auth,"command":{"kind":"code_publications"}})).await;
            if reply["result"]["status"]=="ok" {
                let values=&reply["result"]["value"]["publications"];
                if values.as_array().is_some_and(|values|values.is_empty()!=nonempty) {break values.clone();}
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.unwrap()
}
#[tokio::test]
async fn code_listener_recovers_publishes_joins_and_retires_consumed_intent() {
    let (a, b) = (State::new(), State::new());
    enabled(&a, "amber");
    enabled(&b, "birch");
    codes(&a);
    codes(&b);
    let mut left = a.start().await;
    let mut ac = a.connect().await;
    let aa = ac.register("terminal", &a.0).await;
    control(&mut ac, &aa, json!({"kind":"form","name":"code-team"})).await;
    let issued = control(
        &mut ac,
        &aa,
        json!({"kind":"invite","hours":1,"short":true}),
    )
    .await;
    let before = publication(&mut ac, &aa, true).await;
    assert_eq!(before[0]["intent"], issued["intent"]);
    // Recovery must use durable intent and original endpoint identity, not the
    // caller's invitation bytes or a seeded team document.
    left.kill().await.unwrap();
    drop(ac);
    let mut left = a.start().await;
    let mut ac = a.connect().await;
    assert_eq!(
        ac.request(json!({"id":52,"op":"attach","auth":aa})).await["result"]["status"],
        "ok"
    );
    let recovered = publication(&mut ac, &aa, true).await;
    assert_eq!(before[0]["endpoint"]["id"], recovered[0]["endpoint"]["id"]);
    let mut right = b.start().await;
    let mut bc = b.connect().await;
    let ba = bc.register("terminal", &b.0).await;
    control(
        &mut bc,
        &ba,
        json!({"kind":"join_code","code":issued["code"],"address":recovered[0]["endpoint"]}),
    )
    .await;
    publication(&mut ac, &aa, false).await;
    // A code-derived listener is not a TEAM member; connect using the original
    // authenticated team endpoint after the anchored empty-team bootstrap.
    let endpoint =
        control(&mut ac, &aa, json!({"kind":"status"})).await["status"]["endpoint"].clone();
    control(&mut bc, &ba, json!({"kind":"connect","address":endpoint})).await;
    remote(&mut ac, "birch").await;
    remote(&mut bc, "amber").await;
    deliver(&mut ac, &aa, &mut bc, &ba, "birch", "code-delivery").await;
    ac.request(json!({"id":53,"op":"shutdown","auth":aa})).await;
    bc.request(json!({"id":53,"op":"shutdown","auth":ba})).await;
    exited(&mut left).await;
    exited(&mut right).await;
    let mut left = a.start().await;
    let mut ac = a.connect().await;
    ac.request(json!({"id":54,"op":"attach","auth":aa})).await;
    assert!(
        publication(&mut ac, &aa, false)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    ac.request(json!({"id":55,"op":"shutdown","auth":aa})).await;
    exited(&mut left).await;
}

#[tokio::test]
async fn ordinary_mutations_during_listener_reconciliation_keep_the_daemon_and_current_publication_alive()
 {
    let state = State::new();
    enabled(&state, "amber");
    codes(&state);
    let mut child = state.start().await;
    let (mut client, mut mutator) = (state.connect().await, state.connect().await);
    let auth = client.register("terminal", &state.0).await;
    let other = mutator.register("mutator", &state.0).await;
    control(
        &mut client,
        &auth,
        json!({"kind":"form","name":"generation-turnover"}),
    )
    .await;
    let original = control(&mut client, &auth, json!({"kind":"status"})).await;
    control(
        &mut client,
        &auth,
        json!({"kind":"invite","hours":1,"short":true}),
    )
    .await;
    let mutations = async {
        for _ in 0..20 {
            control(&mut mutator, &other, json!({"kind":"invite","hours":1})).await;
            tokio::task::yield_now().await;
        }
    };
    let publications = async {
        for _ in 0..20 {
            let reply=client.request(json!({"id":71,"op":"network","auth":auth,"command":{"kind":"code_publications"}})).await;
            if reply["result"]["status"] == "error" {
                assert!(matches!(
                    reply["result"]["value"].as_str(),
                    Some("busy" | "rejected")
                ));
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::join!(mutations, publications);
    let ready = publication(&mut client, &auth, true).await;
    assert_eq!(ready.as_array().unwrap().len(), 1);
    let current = control(&mut client, &auth, json!({"kind":"status"})).await;
    assert_eq!(
        current["status"]["endpoint"]["id"],
        original["status"]["endpoint"]["id"]
    );
    assert_eq!(current["status"]["team"], original["status"]["team"]);
    client
        .request(json!({"id":72,"op":"shutdown","auth":auth}))
        .await;
    exited(&mut child).await;
}
