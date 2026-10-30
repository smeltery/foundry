use super::{
    child::{Node, fails_to_start},
    config::Root,
    flow::*,
};
use iroh_relay::server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig, testing};
use serde_json::json;
use std::net::Ipv4Addr;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_children_exchange_through_only_the_explicit_verified_private_tls_relay() {
    let (certificates, tls) = testing::self_signed_tls_certs_and_config();
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config: tls },
    ));
    let mut settings = ServerConfig::default();
    settings.relay = Some(relay);
    let relay = Server::spawn(settings).await.unwrap();
    let url = format!("https://{}", relay.https_addr().unwrap());
    let roots: Vec<_> = certificates.into_iter().map(|c| c.to_vec()).collect();
    let root = Root::new();
    let mut cfg_a = root.config("a", "alex");
    cfg_a.relay_only(&url, &roots);
    let a = Node::start(cfg_a).await;
    let (mut ca, aa) = a.client().await;
    form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, false).await;
    let mut cfg_b = root.config("b", "blair");
    cfg_b.relay_only(&url, &roots);
    seed(&mut cfg_b, &invitation.endpoint);
    let b = Node::start(cfg_b).await;
    let (mut cb, ab) = b.client().await;
    join(&mut cb, &ab, &invitation).await;
    for (client, auth) in [(&mut ca, &aa), (&mut cb, &ab)] {
        let address: iroh::EndpointAddr =
            serde_json::from_value(client.status(auth).await["endpoint"].clone()).unwrap();
        assert_eq!(
            address.ip_addrs().count(),
            0,
            "relay-only daemon advertised direct paths"
        );
        assert!(
            address
                .relay_urls()
                .all(|r| r.to_string() == format!("{url}/"))
        );
    }
    wait_remote(&mut ca, &b.config, true).await;
    wait_remote(&mut cb, &a.config, true).await;
    send(
        &mut ca,
        &aa,
        &b.config,
        "tls-a",
        "private TLS relay forward",
    )
    .await;
    peek(&mut cb, &ab, &[("tls-a", "private TLS relay forward")]).await;
    send(&mut cb, &ab, &a.config, "tls-b", "private TLS relay return").await;
    peek(&mut ca, &aa, &[("tls-b", "private TLS relay return")]).await;
    consume(&mut cb, &ab, &[("tls-a", "private TLS relay forward")]).await;
    consume(&mut ca, &aa, &[("tls-b", "private TLS relay return")]).await;
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
    relay.shutdown().await.unwrap();
    // With the only selected relay stopped and direct disabled, no replacement
    // provider is configured. A fresh Form relay probe must fail, not fall back.
    let mut cfg = root.config("c", "casey");
    cfg.relay_only(&url, &roots);
    cfg.value["network"]["startup"] = json!({"mode":"form","name":"no fallback","relay":url});
    fails_to_start(&cfg).await;
}
