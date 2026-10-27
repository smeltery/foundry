mod common;
use common::config;
use foundry_network::{
    Channel, NetworkEndpoint, NetworkError, PeerFrame, RelaySelection, read_frame, write_frame,
};
use iroh::{EndpointAddr, RelayUrl, endpoint::Connection};
use iroh_relay::server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig, testing};
use std::{error::Error, net::Ipv4Addr, time::Duration};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(15);

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

struct Observations {
    remote_ids_match: bool,
    frames_match: bool,
    client_ip: bool,
    client_relay: bool,
    server_ip: bool,
    server_relay: bool,
}

async fn exchange(left: &NetworkEndpoint, right: &NetworkEndpoint) -> TestResult<Observations> {
    let (outgoing, incoming) = tokio::join!(
        left.connect(right.address(), Channel::Team, DEADLINE),
        right.accept(DEADLINE),
    );
    let client = outgoing?;
    let server = incoming?;
    let frame = PeerFrame::Hello {
        team: "local-test".into(),
        proto: 1,
    };
    let expected = frame.clone();
    let client_side = async {
        let (mut send, mut receive) = client.open_bi().await?;
        write_frame(&mut send, &frame, DEADLINE).await?;
        send.finish()?;
        let received = read_frame(&mut receive, DEADLINE).await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(received)
    };
    let server_side = async {
        let (mut send, mut receive) = server.accept_bi().await?;
        let received = read_frame(&mut receive, DEADLINE).await?;
        let eof = read_frame(&mut receive, DEADLINE).await?;
        if let Some(frame) = &received {
            write_frame(&mut send, frame, DEADLINE).await?;
        }
        send.finish()?;
        Ok::<_, Box<dyn Error + Send + Sync>>((received, eof))
    };
    let (client_result, server_result) = tokio::join!(client_side, server_side);
    let observations = Observations {
        remote_ids_match: client.remote_id() == right.id() && server.remote_id() == left.id(),
        frames_match: client_result
            .as_ref()
            .is_ok_and(|frame| frame.as_ref() == Some(&expected))
            && server_result
                .as_ref()
                .is_ok_and(|(frame, eof)| frame.as_ref() == Some(&expected) && eof.is_none()),
        client_ip: client.paths().iter().any(|path| path.is_ip()),
        client_relay: client.paths().iter().any(|path| path.is_relay()),
        server_ip: server.paths().iter().any(|path| path.is_ip()),
        server_relay: server.paths().iter().any(|path| path.is_relay()),
    };
    client.close(0u32.into(), b"test complete");
    server.close(0u32.into(), b"test complete");
    client_result?;
    server_result?;
    Ok(observations)
}

async fn close_pair(left: NetworkEndpoint, right: NetworkEndpoint) -> TestResult {
    timeout(DEADLINE, async {
        tokio::join!(left.shutdown(), right.shutdown());
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn net_own_001_real_direct_quic_authentication_and_frames() -> TestResult {
    let left = NetworkEndpoint::bind(config()).await?;
    let right = NetworkEndpoint::bind(config()).await?;
    let observed = timeout(DEADLINE, exchange(&left, &right)).await;
    close_pair(left, right).await?;
    let observed = observed??;
    assert!(observed.remote_ids_match && observed.frames_match);
    assert!(observed.client_ip && observed.server_ip);
    assert!(!observed.client_relay && !observed.server_relay);
    Ok(())
}

async fn local_relay() -> TestResult<(Server, RelayUrl, Vec<Vec<u8>>)> {
    let (certificates, tls) = testing::self_signed_tls_certs_and_config();
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config: tls },
    ));
    let mut server_config = ServerConfig::default();
    server_config.relay = Some(relay);
    let server = Server::spawn(server_config).await?;
    let url = format!(
        "https://{}",
        server.https_addr().ok_or("missing TLS address")?
    )
    .parse()?;
    Ok((
        server,
        url,
        certificates.into_iter().map(|cert| cert.to_vec()).collect(),
    ))
}

#[tokio::test]
async fn net_own_002_forced_tls_relay_with_verified_certificate() -> TestResult {
    let (relay, url, roots) = local_relay().await?;
    let exercise = async {
        let mut left_config = config();
        left_config.relay = RelaySelection::Custom(vec![url.clone()]);
        left_config.relay_root_certificates = roots.clone();
        left_config.direct_paths = false;
        let mut right_config = config();
        right_config.relay = RelaySelection::Custom(vec![url]);
        right_config.relay_root_certificates = roots;
        right_config.direct_paths = false;
        let left = NetworkEndpoint::bind(left_config).await?;
        let right = match NetworkEndpoint::bind(right_config).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                left.shutdown().await;
                return Err(error.into());
            }
        };
        let result = timeout(DEADLINE, async {
            tokio::try_join!(left.online(DEADLINE), right.online(DEADLINE))?;
            exchange(&left, &right).await
        })
        .await;
        close_pair(left, right).await?;
        result?
    }
    .await;
    timeout(DEADLINE, relay.shutdown()).await??;
    let observed = exercise?;
    assert!(observed.remote_ids_match && observed.frames_match);
    assert!(observed.client_relay && observed.server_relay);
    assert!(!observed.client_ip && !observed.server_ip);
    Ok(())
}

#[tokio::test]
async fn incompatible_alpn_fails_and_idle_accept_has_deadline() -> TestResult {
    let left = NetworkEndpoint::bind(config()).await?;
    let mut incompatible = config();
    incompatible.profile.team_alpn = b"fixture/other/1".to_vec();
    let right = NetworkEndpoint::bind(incompatible).await?;
    let (dial, accept) = tokio::join!(
        left.connect(right.address(), Channel::Team, Duration::from_millis(200)),
        right.accept(Duration::from_millis(200)),
    );
    let idle = left.accept(Duration::from_millis(10)).await;
    close_pair(left, right).await?;
    assert!(dial.is_err() && accept.is_err());
    assert!(matches!(idle, Err(NetworkError::TimedOut)));
    Ok(())
}

#[tokio::test]
async fn closing_endpoint_closes_live_connections() -> TestResult {
    let left = NetworkEndpoint::bind(config()).await?;
    let right = NetworkEndpoint::bind(config()).await?;
    let address = right.address();
    let (client, server) = tokio::join!(
        left.connect(address, Channel::Join, DEADLINE),
        right.accept(DEADLINE)
    );
    let client: Connection = client?;
    let server = server?;
    timeout(DEADLINE, right.shutdown()).await?;
    timeout(DEADLINE, client.closed()).await?;
    timeout(DEADLINE, server.closed()).await?;
    timeout(DEADLINE, left.shutdown()).await?;
    Ok(())
}

#[tokio::test]
async fn unknown_peer_without_addresses_times_out_without_discovery() -> TestResult {
    let left = NetworkEndpoint::bind(config()).await?;
    let unknown = EndpointAddr::new(iroh::SecretKey::generate().public());
    let result = left
        .connect(unknown, Channel::Team, Duration::from_millis(50))
        .await;
    timeout(DEADLINE, left.shutdown()).await?;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn relay_loss_cannot_fall_back_to_direct_transport() -> TestResult {
    let (relay, url, roots) = local_relay().await?;
    let make_config = || {
        let mut settings = config();
        settings.relay = RelaySelection::Custom(vec![url.clone()]);
        settings.relay_root_certificates = roots.clone();
        settings.direct_paths = false;
        settings
    };
    let left = NetworkEndpoint::bind(make_config()).await?;
    let right = NetworkEndpoint::bind(make_config()).await?;
    tokio::try_join!(left.online(DEADLINE), right.online(DEADLINE))?;
    let (client, server) = tokio::join!(
        left.connect(right.address(), Channel::Team, DEADLINE),
        right.accept(DEADLINE)
    );
    let client = client?;
    let server = server?;
    let (mut send, _) = client.open_bi().await?;
    let frame = PeerFrame::Hello {
        team: "before-loss".into(),
        proto: 1,
    };
    write_frame(&mut send, &frame, DEADLINE).await?;
    let (_, mut receive) = timeout(DEADLINE, server.accept_bi()).await??;
    assert_eq!(read_frame(&mut receive, DEADLINE).await?, Some(frame));
    timeout(DEADLINE, relay.shutdown()).await??;
    let frame = PeerFrame::Hello {
        team: "after-loss".into(),
        proto: 1,
    };
    // QUIC may buffer a successful write; only received bytes prove delivery.
    let _write = write_frame(&mut send, &frame, Duration::from_millis(200)).await;
    let received = read_frame(&mut receive, Duration::from_millis(200)).await;
    let direct = client.paths().iter().any(|path| path.is_ip())
        || server.paths().iter().any(|path| path.is_ip());
    client.close(0u32.into(), b"test complete");
    server.close(0u32.into(), b"test complete");
    close_pair(left, right).await?;
    assert!(
        received.is_err(),
        "relay loss must not report delivered data"
    );
    assert!(!direct);
    Ok(())
}
