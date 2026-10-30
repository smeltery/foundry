use super::*;
use foundry_network::ipc_codec::write_record;
use rusqlite::{Connection, OpenFlags};
use tokio::net::UdpSocket;

fn generation(root: &State, peer: iroh::EndpointId) -> Vec<u8> {
    Connection::open_with_flags(
        root.0.join("mail.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap()
    .query_row(
        "SELECT generation FROM remote_presence WHERE peer=?1 AND online=1",
        [peer.as_bytes()],
        |row| row.get(0),
    )
    .unwrap()
}
async fn setup(
    a: &State,
    b: &State,
    ac: &mut Client,
    bc: &mut Client,
) -> (Value, Value, iroh::EndpointAddr, iroh::EndpointAddr) {
    let aa = ac.register("terminal", &a.0).await;
    let ba = bc.register("terminal", &b.0).await;
    control(ac, &aa, json!({"kind":"form","name":"connection-fixture"})).await;
    let invite = control(ac, &aa, json!({"kind":"invite","hours":1})).await;
    control(
        bc,
        &ba,
        json!({"kind":"join","ticket":invite["ticket"],"address":invite["endpoint"]}),
    )
    .await;
    remote(ac, "birch").await;
    remote(bc, "amber").await;
    let left = control(ac, &aa, json!({"kind":"status"})).await;
    let right = control(bc, &ba, json!({"kind":"status"})).await;
    (
        aa,
        ba,
        serde_json::from_value(left["status"]["endpoint"].clone()).unwrap(),
        serde_json::from_value(right["status"]["endpoint"].clone()).unwrap(),
    )
}
#[tokio::test]
async fn repeated_simultaneous_connect_preserves_live_generation_and_refuses_unadmitted_peer() {
    let (a, b) = (State::new(), State::new());
    enabled(&a, "amber");
    enabled(&b, "birch");
    let (mut left, mut right) = (a.start().await, b.start().await);
    let (mut ac, mut bc) = (a.connect().await, b.connect().await);
    let (aa, ba, aaddr, baddr) = setup(&a, &b, &mut ac, &mut bc).await;
    let before = (generation(&a, baddr.id), generation(&b, aaddr.id));
    for _ in 0..3 {
        let (ar,br)=tokio::join!(
            ac.request(json!({"id":101,"op":"network","auth":aa,"command":{"kind":"connect","address":baddr}})),
            bc.request(json!({"id":101,"op":"network","auth":ba,"command":{"kind":"connect","address":aaddr}}))
        );
        assert_eq!(
            ar["result"]["status"], "ok",
            "admitted connect request rejected"
        );
        assert_eq!(
            br["result"]["status"], "ok",
            "admitted connect request rejected"
        );
    }
    let unrelated = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr("127.0.0.1:9".parse().unwrap());
    let refused=ac.request(json!({"id":102,"op":"network","auth":aa,"command":{"kind":"connect","address":unrelated}})).await;
    assert_eq!(refused["result"]["status"], "error");
    remote(&mut ac, "birch").await;
    remote(&mut bc, "amber").await;
    assert_eq!(
        generation(&a, baddr.id),
        before.0,
        "healthy driver was replaced"
    );
    assert_eq!(
        generation(&b, aaddr.id),
        before.1,
        "healthy driver was replaced"
    );
    deliver(
        &mut ac,
        &aa,
        &mut bc,
        &ba,
        "birch",
        "after-idempotent-connect",
    )
    .await;
    ac.request(json!({"id":103,"op":"shutdown","auth":aa}))
        .await;
    bc.request(json!({"id":103,"op":"shutdown","auth":ba}))
        .await;
    exited(&mut left).await;
    exited(&mut right).await;
}

async fn offline(client: &mut Client, person: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .request(json!({"id":104,"op":"who","filter":{"person":person}}))
                .await;
            if reply["result"]["value"]["entries"]
                .as_array()
                .is_some_and(|entries| {
                    !entries.is_empty() && entries.iter().all(|entry| entry["online"] == false)
                })
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn accepted_address_update_during_observed_dial_is_used_by_eventual_retry() {
    let (a, b) = (State::new(), State::new());
    enabled(&a, "amber");
    enabled(&b, "birch");
    let (mut left, mut right) = (a.start().await, b.start().await);
    let (mut ac, mut bc) = (a.connect().await, b.connect().await);
    let (aa, ba, _, baddr) = setup(&a, &b, &mut ac, &mut bc).await;
    bc.request(json!({"id":105,"op":"shutdown","auth":ba}))
        .await;
    exited(&mut right).await;
    drop(bc);
    offline(&mut ac, "birch").await;
    let blackhole = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let unavailable =
        iroh::EndpointAddr::new(baddr.id).with_ip_addr(blackhole.local_addr().unwrap());
    // Send without awaiting a handshake-complete response: actual UDP reception
    // establishes that the supervisor's selected outgoing attempt is in flight.
    write_record(ac.0.get_mut(),&json!({"id":106,"op":"network","auth":aa,"command":{"kind":"connect","address":unavailable}}),DEADLINE).await.unwrap();
    let mut packet = [0; 2048];
    tokio::time::timeout(Duration::from_secs(15), blackhole.recv_from(&mut packet))
        .await
        .unwrap()
        .unwrap();
    let accepted = ac.receive().await;
    assert_eq!(accepted["id"], 106);
    assert_eq!(accepted["result"]["status"], "ok");
    let mut right = b.start().await;
    let mut bc = b.connect().await;
    bc.request(json!({"id":107,"op":"attach","auth":ba})).await;
    let current = control(&mut bc, &ba, json!({"kind":"status"})).await;
    let updated=tokio::time::timeout(Duration::from_secs(2),ac.request(json!({"id":108,"op":"network","auth":aa,"command":{"kind":"connect","address":current["status"]["endpoint"]}}))).await.unwrap();
    assert_eq!(
        updated["result"]["status"], "ok",
        "an in-flight dial blocked acceptance of a new address"
    );
    remote(&mut ac, "birch").await;
    remote(&mut bc, "amber").await;
    deliver(&mut ac, &aa, &mut bc, &ba, "birch", "after-address-update").await;
    ac.request(json!({"id":109,"op":"shutdown","auth":aa}))
        .await;
    bc.request(json!({"id":109,"op":"shutdown","auth":ba}))
        .await;
    exited(&mut left).await;
    exited(&mut right).await;
}
