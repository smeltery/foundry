use super::*;
use crate::{
    Channel,
    endpoint::AcceptedConnection,
    join_protocol::{Refusal, serve_join},
    peer_session::{AdmissionFence, PeerSession, SessionOptions},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

/// The one-second accept window bounds only waiting for a peer. Each handshake
/// then runs in the task set under the per-connection DEADLINE and counts
/// toward MAX_CONNECTING, so neither the window nor a competing wakeup drops it.
pub(super) async fn run(
    state: Arc<State>,
    sessions: mpsc::Sender<connections::Ready>,
) -> Result<(), ControlError> {
    let stop = state.stop.subscribe();
    let mut tasks = JoinSet::new();
    // Set when routing ends, however it ends, so in-flight work stops too.
    let (halt, halted) = watch::channel(false);
    let mut failure = None;
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = stopped(stop.clone()) => break,
            result = tasks.join_next(), if !tasks.is_empty() => {
                match result {
                    Some(Ok(())) => state.wake.notify_one(),
                    Some(Err(_)) => { failure = Some(ControlError::Storage); break; }
                    None => {},
                }
            }
            incoming = state.endpoint.incoming(Duration::from_secs(1)) => {
                let handshake = match incoming {
                    Ok(handshake) => handshake,
                    Err(crate::NetworkError::TimedOut) => continue,
                    Err(_) => { failure = Some(ControlError::Transport); break; }
                };
                if tasks.len() >= MAX_CONNECTING { handshake.refuse(); continue; }
                let owner = state.clone();
                let send = sessions.clone();
                let halted = halted.clone();
                tasks.spawn(async move {
                    let accepted = tokio::select! {
                        biased;
                        _ = stopped(halted.clone()) => return,
                        accepted = handshake.complete_routed(DEADLINE) => accepted,
                    };
                    // A failed handshake or refused channel belongs to that peer.
                    let Ok(accepted) = accepted else { return };
                    match accepted.channel() {
                        Channel::Team => serve_team(owner, accepted, send).await,
                        Channel::Join => serve_join_until(owner, accepted, halted).await,
                    }
                });
            }
        }
    }
    halt.send_replace(true);
    while tasks.join_next().await.is_some() {}
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn serve_team(
    owner: Arc<State>,
    accepted: AcceptedConnection,
    sessions: mpsc::Sender<connections::Ready>,
) {
    let Some(reservation) =
        owner
            .connections
            .reserve(owner.endpoint.id(), accepted.remote_id(), false)
    else {
        return;
    };
    let options = SessionOptions {
        profile: owner.settings.profile.clone(),
        deadline: DEADLINE,
        cancel: reservation.cancel(),
    };
    let result = tokio::select! {
        biased;
        _ = stopped(owner.stop.subscribe()) => return,
        result = PeerSession::accept_connection(&owner.endpoint, accepted, owner.authority.clone(), options) => result,
    };
    if let Ok(session) = result {
        tokio::select! {
            biased;
            _ = stopped(owner.stop.subscribe()) => {},
            _ = sessions.send(connections::Ready { session, reservation }) => {},
        }
    }
}

async fn serve_join_until(
    owner: Arc<State>,
    accepted: AcceptedConnection,
    halted: watch::Receiver<bool>,
) {
    let Ok(connection) = owner.endpoint.into_connection(accepted) else {
        return;
    };
    let fence = Arc::new(AdmissionFence::new(owner.stop.subscribe()));
    let authority = owner.authority.clone();
    let admission = fence.clone();
    let serving = serve_join(
        connection.clone(),
        &owner.settings.profile,
        DEADLINE,
        move |peer, request| async move {
            authority
                .admit_join_fenced(peer, request, admission)
                .await
                .map_err(|_| Refusal::Denied)
        },
    );
    tokio::pin!(serving);
    tokio::select! {
        biased;
        _ = &mut serving => {}
        _ = stopped(halted) => {
            // A commit that already won the fence keeps its guarded reply.
            if fence.stop_pending() {
                connection.close(0u32.into(), b"host stopped");
            }
            let _ = serving.await;
        }
    }
}
