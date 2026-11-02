use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn pending_join_stop_and_caller_drop_fence_blocked_store_and_retain_actual_lease() {
    for abort_caller in [false, true] {
        let (a, b) = (Node::new(), Node::new());
        form(&a, "alice");
        let (entered, at_store) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let armed = Arc::new(AtomicBool::new(false));
        let in_clock = armed.clone();
        let authority = AdmittedTeam::new(
            a.store(),
            ownership(&a),
            a.identity.endpoint_id(),
            move || {
                if in_clock.load(Ordering::Acquire)
                    && let Some(entered) = entered.lock().unwrap().take()
                {
                    let _ = entered.send(());
                }
                Ok(NOW + 100)
            },
            None,
        )
        .unwrap();
        let invitation = authority
            .issue_invite("blocked-access".into(), 1, false)
            .await
            .unwrap();
        let team = invitation.state().id.clone();
        drop(invitation);
        let blocking = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(a.root.join(full_team_store::FULL_TEAM_LOCK))
            .unwrap();
        blocking.lock().unwrap();
        let (keep, cancel) = watch::channel(false);
        let fence = Arc::new(AdmissionFence::new(cancel));
        let worker = authority.clone();
        let worker_fence = fence.clone();
        let peer = b.identity.endpoint_id();
        armed.store(true, Ordering::Release);
        let pending = tokio::spawn(async move {
            worker
                .admit_join_fenced(
                    peer,
                    team::JoinRequest {
                        team,
                        secret: "blocked-access".into(),
                        person: "bob".into(),
                        machine: "desktop".into(),
                        proof: None,
                        owner_only: false,
                    },
                    worker_fence,
                )
                .await
        });
        let mut pending = Some(pending);
        tokio::time::timeout(WAIT, at_store).await.unwrap().unwrap();
        if abort_caller {
            let task = pending.take().unwrap();
            task.abort();
            match task.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("cancelled admission completed"),
            };
        } else {
            assert!(fence.stop_pending());
        }
        drop(authority);
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&a.root, cap_std::ambient_authority()).unwrap();
        assert!(matches!(
            daemon_lease::DaemonLease::acquire(&directory),
            Err(daemon_lease::LeaseError::AlreadyRunning)
        ));
        blocking.unlock().unwrap();
        if !abort_caller {
            assert!(matches!(
                pending.take().unwrap().await.unwrap(),
                Err(PeerError::Closed)
            ));
        }
        let reacquire = async {
            loop {
                match daemon_lease::DaemonLease::acquire(&directory) {
                    Ok(owner) => break owner,
                    Err(daemon_lease::LeaseError::AlreadyRunning) => tokio::task::yield_now().await,
                    Err(_) => panic!("lease error"),
                }
            }
        };
        let owner = tokio::time::timeout(WAIT, reacquire).await.unwrap();
        let state = a.store().load(NOW + 100).unwrap();
        assert!(
            !state
                .document()
                .unwrap()
                .members
                .contains_key(&peer.to_string())
        );
        assert!(
            state
                .document()
                .unwrap()
                .invites
                .values()
                .all(|i| i.body.used_by.is_none())
        );
        drop((owner, keep));
    }
}

#[tokio::test]
async fn real_join_blocked_at_store_returns_refusal_after_synchronous_stop() {
    use foundry_network::join_protocol::{JoinResult, Refusal, join_peer, serve_join};
    let (a, b) = (Node::new(), Node::new());
    form(&a, "alice");
    let (entered, at_store) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let armed = Arc::new(AtomicBool::new(false));
    let in_clock = armed.clone();
    let authority = AdmittedTeam::new(
        a.store(),
        ownership(&a),
        a.identity.endpoint_id(),
        move || {
            if in_clock.load(Ordering::Acquire)
                && let Some(entered) = entered.lock().unwrap().take()
            {
                let _ = entered.send(());
            }
            Ok(NOW + 100)
        },
        None,
    )
    .unwrap();
    let invitation = authority
        .issue_invite("network-stop".into(), 1, false)
        .await
        .unwrap();
    let team = invitation.state().id.clone();
    drop(invitation);
    let blocker = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(a.root.join(full_team_store::FULL_TEAM_LOCK))
        .unwrap();
    blocker.lock().unwrap();
    let (left, right) = (Arc::new(a.endpoint().await), Arc::new(b.endpoint().await));
    let (keep, cancel) = watch::channel(false);
    let fence = Arc::new(AdmissionFence::new(cancel));
    let serving = left.clone();
    let accepting = authority.clone();
    let admission = fence.clone();
    armed.store(true, Ordering::Release);
    let server = tokio::spawn(async move {
        let accepted = serving.accept_routed(WAIT).await.unwrap();
        assert!(matches!(accepted.channel(), Channel::Join));
        let connection = serving.into_connection(accepted).unwrap();
        serve_join(
            connection,
            &profile(),
            WAIT,
            move |peer, request| async move {
                accepting
                    .admit_join_fenced(peer, request, admission)
                    .await
                    .map_err(|_| Refusal::Denied)
            },
        )
        .await
        .unwrap()
    });
    let dialing = right.clone();
    let address = left.address();
    let client = tokio::spawn(async move {
        join_peer(
            &dialing,
            address,
            &team::JoinRequest {
                team,
                secret: "network-stop".into(),
                person: "bob".into(),
                machine: "desktop".into(),
                proof: None,
                owner_only: false,
            },
            WAIT,
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(WAIT, at_store).await.unwrap().unwrap();
    assert!(fence.stop_pending());
    blocker.unlock().unwrap();
    assert!(matches!(client.await.unwrap(), JoinResult::Refused(_)));
    assert!(!server.await.unwrap());
    assert!(
        !a.store()
            .load(NOW + 100)
            .unwrap()
            .document()
            .unwrap()
            .members
            .contains_key(&b.identity.endpoint_id().to_string())
    );
    drop((authority, keep));
    tokio::join!(
        Arc::try_unwrap(left).ok().unwrap().shutdown(),
        Arc::try_unwrap(right).ok().unwrap().shutdown()
    );
}
