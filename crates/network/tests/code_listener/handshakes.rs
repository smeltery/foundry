//! A handshake already in progress must outlive the listener's accept window
//! and any competing wakeup of its serving loop.
use super::*;
use crate::handshake_hold::{ACCEPT_WINDOW, HandshakeHold};
use foundry_network::Channel;
use iroh::EndpointAddr;
use tokio::{sync::watch, task::JoinHandle};

struct Serving {
    issuer: Fixture,
    address: EndpointAddr,
    stop: watch::Sender<bool>,
    serving: JoinHandle<Result<CodeExit, foundry_network::peer_session::PeerError>>,
    client: NetworkEndpoint,
}
impl Serving {
    async fn start() -> Self {
        let issuer = Fixture::new();
        let authority = authority(&issuer, Arc::new(AtomicI64::new(NOW)));
        drop(authority.issue_code(1, false).await.unwrap());
        let lease = authority
            .recover_code_intents()
            .await
            .unwrap()
            .pop()
            .unwrap();
        let listener = CodeListener::bind(authority, lease, transport())
            .await
            .unwrap();
        let address = listener.publication().await.unwrap().address().clone();
        let (stop, cancel) = watch::channel(false);
        let serving = tokio::spawn(listener.serve(cancel));
        let client = endpoint(&issuer).await;
        Self {
            issuer,
            address,
            stop,
            serving,
            client,
        }
    }
    async fn refused_join(&self, address: EndpointAddr) -> JoinResult {
        refused_join(&self.client, address).await
    }
    async fn finish(self) {
        self.stop.send(true).unwrap();
        let exit = tokio::time::timeout(DEADLINE, self.serving).await.unwrap();
        assert_eq!(exit.unwrap().unwrap(), CodeExit::Stopped);
        self.client.shutdown().await;
    }
}
fn wrong_code() -> JoinRequest {
    JoinRequest {
        team: String::new(),
        secret: "not-the-issued-code".into(),
        person: "bob".into(),
        machine: "laptop".into(),
        proof: None,
        owner_only: false,
    }
}
/// A refusal proves the listener completed the handshake and served it.
async fn refused_join(client: &NetworkEndpoint, address: EndpointAddr) -> JoinResult {
    join_peer(client, address, &wrong_code(), DEADLINE)
        .await
        .unwrap()
}
async fn endpoint(issuer: &Fixture) -> NetworkEndpoint {
    let mut config = common::config();
    config.profile = issuer.base.profile.clone();
    NetworkEndpoint::bind(config).await.unwrap()
}
fn assert_refused(result: JoinResult) {
    let JoinResult::Refused(refusal) = result else {
        panic!("a wrong code must be refused")
    };
    assert_eq!(refusal, "invitation refused");
}

#[tokio::test]
async fn handshake_held_past_the_accept_window_still_completes() {
    let serving = Serving::start().await;
    let hold = HandshakeHold::start(&serving.address).await;
    let result = {
        let joining = serving.refused_join(hold.address(&serving.address));
        tokio::pin!(joining);
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = hold.stalled() => {}
        }
        // The window that admitted this client has certainly expired after one
        // full window; only the handshake's own deadline may end it now.
        let crossed = tokio::time::sleep(ACCEPT_WINDOW + Duration::from_millis(250));
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = crossed => hold.release(),
        }
        joining.await
    };
    assert_refused(result);
    serving.finish().await;
}

#[tokio::test]
async fn handshake_survives_a_competing_task_completion_mid_flight() {
    let serving = Serving::start().await;
    // An idle JOIN connection owns a listener task until it sends a bad frame.
    // It uses its own endpoint so the held client learns no direct path.
    let other = endpoint(&serving.issuer).await;
    let idle = other
        .connect(serving.address.clone(), Channel::Join, DEADLINE)
        .await
        .unwrap();
    let hold = HandshakeHold::start(&serving.address).await;
    let result = {
        let joining = serving.refused_join(hold.address(&serving.address));
        tokio::pin!(joining);
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = hold.stalled() => {}
        }
        let (mut send, _receive) = idle.open_bi().await.unwrap();
        send.write_all(&[0, 0]).await.unwrap();
        send.finish().unwrap();
        // The listener closes the idle connection as its task ends, waking the
        // serving loop's task-completion branch while the other handshake is held.
        let closed = tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            closed = tokio::time::timeout(DEADLINE, idle.closed()) => closed.unwrap(),
        };
        assert!(
            closed.to_string().contains("join exchange finished"),
            "{closed}"
        );
        hold.release();
        joining.await
    };
    assert_refused(result);
    serving.finish().await;
    other.shutdown().await;
}

#[tokio::test]
async fn in_flight_handshakes_count_toward_invitation_capacity() {
    let serving = Serving::start().await;
    let mut held = Vec::new();
    for _ in 0..8 {
        let hold = HandshakeHold::start(&serving.address).await;
        let client = endpoint(&serving.issuer).await;
        let address = hold.address(&serving.address);
        let joining = tokio::spawn(async move {
            let result = refused_join(&client, address).await;
            client.shutdown().await;
            result
        });
        hold.stalled().await;
        held.push((hold, joining));
    }
    // Eight stalled handshakes fill the listener; a ninth is refused unserved.
    let extra = endpoint(&serving.issuer).await;
    let refused = extra
        .connect(serving.address.clone(), Channel::Join, DEADLINE)
        .await
        .expect_err("capacity must include in-flight handshakes");
    assert!(refused.to_string().contains("refused"), "{refused}");
    for (hold, joining) in held {
        hold.release();
        assert_refused(joining.await.unwrap());
    }
    extra.shutdown().await;
    serving.finish().await;
}

#[tokio::test]
async fn stop_abandons_a_held_handshake_without_waiting_for_its_deadline() {
    let serving = Serving::start().await;
    let hold = HandshakeHold::start(&serving.address).await;
    let address = hold.address(&serving.address);
    let client = endpoint(&serving.issuer).await;
    let joining = tokio::spawn(async move {
        let result = join_peer(&client, address, &wrong_code(), DEADLINE).await;
        client.shutdown().await;
        result.is_ok()
    });
    hold.stalled().await;
    let stopping = std::time::Instant::now();
    serving.finish().await;
    // The listener timeout is the handshake deadline; stop must not wait it out.
    assert!(stopping.elapsed() < DEADLINE, "{:?}", stopping.elapsed());
    hold.release();
    assert!(!joining.await.unwrap());
}
