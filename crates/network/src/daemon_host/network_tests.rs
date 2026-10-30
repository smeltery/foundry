use super::*;
use crate::{
    WireProfile,
    daemon_host::response::tests::State,
    full_team_store::{FullTeamStore, StorePolicy},
    peer_session::AuthorityStatus,
};

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking operation did not reach the ownership boundary");
}

async fn authority(state: &State, engine: &OwnedEngine) -> Arc<AdmittedTeam> {
    let identity =
        Arc::new(crate::load_or_create_identity(&state.root(), Duration::from_secs(5)).unwrap());
    let profile = WireProfile {
        team_alpn: b"guard-fixture/team/1".to_vec(),
        join_alpn: b"guard-fixture/join/1".to_vec(),
        protocol: 1,
        signing_prefix: "guard-fixture/sign/".into(),
        invitation_endpoint_context: "guard fixture endpoint seed".into(),
        invitation_secret_context: "guard fixture secret seed".into(),
        ticket_prefixes: vec!["gf1-".into()],
    };
    let store = FullTeamStore::create_new(
        &state.0,
        identity.clone(),
        StorePolicy::new(profile, None).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let authority = AdmittedTeam::open(
        store,
        engine._lease.retain().unwrap(),
        identity.endpoint_id(),
        || Ok(8_000_000),
        None,
    )
    .unwrap();
    drop(
        authority
            .form("guard fixture".into(), "alice".into(), "desktop".into())
            .await
            .unwrap(),
    );
    authority
}

#[tokio::test]
async fn aborted_sync_retains_actual_authority_until_blocked_engine_work_drains() {
    let state = State::new();
    let engine = state.engine();
    let access = EngineAccess::new(engine.clone());
    let authority = authority(&state, &engine).await;
    let view = authority.routing_view().await.unwrap();
    let (locked, acquired) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let holding = engine.clone();
    let holder = std::thread::spawn(move || {
        let _guard = holding.inner.lock().unwrap();
        locked.send(()).unwrap();
        wait.recv().unwrap();
    });
    acquired.await.unwrap();
    let baseline = Arc::strong_count(&engine);
    let worker = access.clone();
    let syncing = tokio::spawn(async move { worker.sync(view, None).await });
    // The async task and with()'s blocking owner must both exist. The Engine
    // mutex prevents that owner from completing before cancellation is tested.
    until(|| Arc::strong_count(&engine) == baseline + 2).await;
    syncing.abort();
    assert!(syncing.await.unwrap_err().is_cancelled());
    assert_eq!(Arc::strong_count(&engine), baseline + 1);

    let progressed_while_blocked =
        tokio::time::timeout(Duration::from_millis(250), authority.status())
            .await
            .is_ok();
    // Release and drain even on the old implementation so a failed assertion
    // does not leave a blocking worker behind during runtime shutdown.
    release.send(()).unwrap();
    holder.join().unwrap();
    until(|| Arc::strong_count(&engine) == 2).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), authority.status())
            .await
            .unwrap()
            .unwrap(),
        AuthorityStatus::Active { .. }
    ));
    access
        .sync(authority.routing_view().await.unwrap(), None)
        .await
        .unwrap();
    assert!(
        !progressed_while_blocked,
        "actual authority progressed after outer cancellation while Engine stayed locked"
    );
}
