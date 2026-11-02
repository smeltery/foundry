use super::*;
use crate::{
    daemon_config::{DaemonConfig, NetworkMode},
    daemon_host::{Daemon, network::EngineAccess},
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).unwrap();
        let root = PathBuf::from("/tmp").join(format!(
            "code-race-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root.canonicalize().unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn open(root: &Root) -> (Daemon, Arc<State>) {
    let bytes = b"amber\ncedar\n";
    fs::write(root.0.join("words"), bytes).unwrap();
    fs::set_permissions(root.0.join("words"), fs::Permissions::from_mode(0o600)).unwrap();
    let mut value = json!({"state_directory":root.0,"socket_directory":root.0,"person":"amber","machine":"desk","folders":[],"network":"disabled"});
    let daemon = Daemon::open(serde_json::from_value::<DaemonConfig>(value.clone()).unwrap())
        .await
        .unwrap();
    value["network"] = json!({"mode":"enabled","profile":{"team_alpn":[114,99,116],"join_alpn":[114,99,106],"protocol":21,
        "signing_prefix":"code-race/","invitation_endpoint_context":"code race endpoint","invitation_secret_context":"code race secret","ticket_prefixes":["race21"]},
        "transport":{"bind_address":"127.0.0.1:0","direct_paths":true,"relays":[],"relay_root_certificates":[]},"seeds":[],"trust":{"mode":"unverified"},
        "codebook":{"path":"words","sha256":data_encoding::HEXLOWER.encode(ring::digest::digest(&ring::digest::SHA256,bytes).as_ref())},"startup":{"mode":"existing"}});
    let config: DaemonConfig = serde_json::from_value(value).unwrap();
    let settings = match config.network {
        NetworkMode::Enabled(network) => network.resolve(&root.0).unwrap(),
        _ => unreachable!(),
    };
    let engine = EngineAccess::for_test(&daemon);
    let settings = Arc::new(settings);
    let (authority, identity) = bootstrap::open(&root.0, engine.clone(), settings.clone())
        .await
        .unwrap();
    let endpoint = Arc::new(
        NetworkEndpoint::bind(settings.endpoint_config(identity.secret_key().clone()))
            .await
            .unwrap(),
    );
    let (stop, _) = watch::channel(false);
    let (connect, _) = tokio::sync::mpsc::channel(32);
    let state = Arc::new(State {
        endpoint,
        authority,
        settings,
        engine,
        addresses: RwLock::new(BTreeMap::new()),
        wake: Notify::new(),
        stop,
        controls: Arc::new(Semaphore::new(32)),
        person: "amber".into(),
        machine: "desk".into(),
        root: root.0.clone(),
        codes: Codes::default(),
        connections: connections::Connections::default(),
        connect,
    });
    (daemon, state)
}
#[tokio::test]
async fn stale_recovered_lease_rescans_and_preserves_authority_without_publishing_old_generation() {
    let root = Root::new();
    let (daemon, state) = open(&root).await;
    drop(
        state
            .authority
            .form("race".into(), "amber".into(), "desk".into())
            .await
            .unwrap(),
    );
    drop(state.authority.issue_code(1, false).await.unwrap());
    let captured = state.authority.recover_code_intents().await.unwrap();
    assert!(!captured.is_empty());
    // Deliberately stop between recovery and bind, then commit a normal mutation.
    drop(
        state
            .authority
            .issue_invite("additional-intent".into(), 1, false)
            .await
            .unwrap(),
    );
    let codes = Codes::default();
    assert_eq!(
        codes.bind_leases(&state, captured).await.unwrap(),
        Reconciled::Retry
    );
    assert!(codes.entries.lock().await.is_empty());
    assert!(!*state.stop.borrow());
    assert!(matches!(
        state.authority.status().await.unwrap(),
        crate::peer_session::AuthorityStatus::Active { .. }
    ));
    assert_eq!(codes.reconcile(&state).await.unwrap(), Reconciled::Current);
    assert!(!codes.entries.lock().await.is_empty());
    codes.shutdown().await.unwrap();
    state.endpoint.close().await;
    drop(state);
    drop(daemon);
}
#[test]
fn expected_turnover_is_distinct_from_storage_transport_and_quiescence() {
    assert_eq!(turnover(PeerError::StaleEpoch).unwrap(), Reconciled::Retry);
    assert_eq!(
        turnover(PeerError::Unauthorized).unwrap(),
        Reconciled::Retry
    );
    assert_eq!(
        turnover(PeerError::Quiescent).unwrap(),
        Reconciled::Quiescent
    );
    assert!(matches!(
        turnover(PeerError::Storage),
        Err(ControlError::Storage)
    ));
    assert!(matches!(
        turnover(PeerError::Transport),
        Err(ControlError::Transport)
    ));
}

#[tokio::test]
async fn superseded_retirement_acknowledgement_retries_the_current_descriptor() {
    let root = Root::new();
    let (daemon, state) = open(&root).await;
    drop(
        state
            .authority
            .form("race".into(), "amber".into(), "desk".into())
            .await
            .unwrap(),
    );
    let issued = state.authority.issue_code(1, false).await.unwrap();
    let policy = state.settings.code_policy().unwrap().unwrap();
    let code = policy.normalize(issued.issued().code()).unwrap();
    drop(issued);
    let lease = state
        .authority
        .recover_code_intents()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let derived = policy.derive(&code).unwrap();
    let request = crate::team::JoinRequest {
        team: String::new(),
        secret: derived.bearer_secret().into(),
        person: "birch".into(),
        machine: "desk".into(),
        proof: None,
        owner_only: false,
    };
    drop(
        state
            .authority
            .admit_code_join(iroh::SecretKey::generate().public(), request, lease)
            .await
            .unwrap(),
    );
    let effect = state
        .authority
        .code_effects()
        .await
        .unwrap()
        .into_iter()
        .find(|e| matches!(e, ControlEffect::RetireCode { .. }))
        .unwrap();
    drop(
        state
            .authority
            .issue_invite("another-invitation".into(), 1, false)
            .await
            .unwrap(),
    );
    let error = state
        .authority
        .acknowledge_code_effect(effect)
        .await
        .unwrap_err();
    assert_eq!(error, PeerError::StaleEpoch);
    let codes = Codes::default();
    assert_eq!(codes.after_error(error).await.unwrap(), Reconciled::Retry);
    assert!(
        state
            .authority
            .code_effects()
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e, ControlEffect::RetireCode { .. }))
    );
    assert_eq!(codes.reconcile(&state).await.unwrap(), Reconciled::Current);
    assert!(
        !state
            .authority
            .code_effects()
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e, ControlEffect::RetireCode { .. }))
    );
    assert!(!*state.stop.borrow());
    state.endpoint.close().await;
    drop(state);
    drop(daemon);
}
