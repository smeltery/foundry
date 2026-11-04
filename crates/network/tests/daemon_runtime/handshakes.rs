//! The daemon router's accept window and task-completion wakeups must not
//! cancel a peer handshake already in progress.
use super::*;
use crate::handshake_hold::{ACCEPT_WINDOW, HandshakeHold};
use foundry_network::{
    Channel, NetworkEndpoint,
    join_protocol::{JoinResult, join_peer},
    team::JoinRequest,
};
use iroh::EndpointAddr;

struct Router {
    state: State,
    child: tokio::process::Child,
    client: Client,
    auth: Value,
    address: EndpointAddr,
}
impl Router {
    async fn start() -> Self {
        let state = State::new();
        enabled(&state, "copper");
        let child = state.start().await;
        let mut client = state.connect().await;
        let auth = client.register("terminal", &state.0).await;
        control(
            &mut client,
            &auth,
            json!({"kind":"form","name":"handshake-fixture"}),
        )
        .await;
        let status = control(&mut client, &auth, json!({"kind":"status"})).await;
        let address = serde_json::from_value(status["status"]["endpoint"].clone()).unwrap();
        Self {
            state,
            child,
            client,
            auth,
            address,
        }
    }
    fn endpoint_config(&self) -> foundry_network::EndpointConfig {
        let config: foundry_network::daemon_config::DaemonConfig =
            serde_json::from_slice(&fs::read(self.state.0.join("config.json")).unwrap()).unwrap();
        let foundry_network::daemon_config::NetworkMode::Enabled(network) = config.network else {
            unreachable!()
        };
        let settings = network.resolve(&self.state.0).unwrap();
        settings.endpoint_config(iroh::SecretKey::generate())
    }
    async fn endpoint(&self) -> NetworkEndpoint {
        NetworkEndpoint::bind(self.endpoint_config()).await.unwrap()
    }
    async fn finish(mut self) {
        self.client
            .request(json!({"id":95,"op":"shutdown","auth":self.auth}))
            .await;
        exited(&mut self.child).await;
    }
}
/// A refusal proves the router completed the handshake and served JOIN.
async fn refused_join(endpoint: &NetworkEndpoint, address: EndpointAddr) -> String {
    let request = JoinRequest {
        team: String::new(),
        secret: "not-an-issued-ticket".into(),
        person: "bob".into(),
        machine: "laptop".into(),
        proof: None,
        owner_only: false,
    };
    match join_peer(endpoint, address, &request, DEADLINE)
        .await
        .unwrap()
    {
        JoinResult::Refused(refusal) => refusal,
        JoinResult::Joined(_) => panic!("an unissued secret must be refused"),
    }
}

#[tokio::test]
async fn router_handshake_held_past_the_accept_window_still_completes() {
    let router = Router::start().await;
    let joiner = router.endpoint().await;
    let hold = HandshakeHold::start(&router.address).await;
    let refusal = {
        let joining = refused_join(&joiner, hold.address(&router.address));
        tokio::pin!(joining);
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = hold.stalled() => {}
        }
        // One full window after the stall, the admitting window has expired.
        let crossed = tokio::time::sleep(ACCEPT_WINDOW + Duration::from_millis(250));
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = crossed => hold.release(),
        }
        joining.await
    };
    assert_eq!(refusal, "invitation refused");
    joiner.shutdown().await;
    router.finish().await;
}

#[tokio::test]
async fn router_handshake_survives_a_competing_task_completion_mid_flight() {
    let router = Router::start().await;
    let (joiner, other) = (router.endpoint().await, router.endpoint().await);
    // An idle JOIN connection owns a router task until it sends a bad frame.
    let idle = other
        .connect(router.address.clone(), Channel::Join, DEADLINE)
        .await
        .unwrap();
    let hold = HandshakeHold::start(&router.address).await;
    let refusal = {
        let joining = refused_join(&joiner, hold.address(&router.address));
        tokio::pin!(joining);
        tokio::select! {
            _ = &mut joining => panic!("join finished while its handshake was held"),
            _ = hold.stalled() => {}
        }
        let (mut send, _receive) = idle.open_bi().await.unwrap();
        send.write_all(&[0, 0]).await.unwrap();
        send.finish().unwrap();
        // The router closes the idle connection as its task ends, waking the
        // task-completion branch while the other handshake is held.
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
    assert_eq!(refusal, "invitation refused");
    tokio::join!(joiner.shutdown(), other.shutdown());
    router.finish().await;
}
