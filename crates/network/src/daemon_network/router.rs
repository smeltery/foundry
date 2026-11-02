use super::*;
use crate::{
    Channel,
    join_protocol::{Refusal, serve_join},
    peer_session::{AdmissionFence, PeerSession, SessionOptions},
};
use tokio::{sync::mpsc, task::JoinSet};

pub(super) async fn run(
    state: Arc<State>,
    sessions: mpsc::Sender<connections::Ready>,
) -> Result<(), ControlError> {
    let stop = state.stop.subscribe();
    let mut tasks = JoinSet::new();
    let mut pending = BTreeMap::new();
    let mut next = 0u64;
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
                    Some(Ok(id)) => { pending.remove(&id); state.wake.notify_one(); }
                    Some(Err(_)) => { failure = Some(ControlError::Storage); break; }
                    None => {},
                }
            }
            accepted = state.endpoint.accept_routed(Duration::from_secs(1)) => {
                let accepted = match accepted {
                    Ok(accepted) => accepted,
                    Err(crate::NetworkError::TimedOut) => continue,
                    Err(_) => { failure = Some(ControlError::Transport); break; }
                };
                if tasks.len() >= MAX_CONNECTING { drop(accepted); continue; }
                let id = next;
                next = next.checked_add(1).ok_or(ControlError::Busy)?;
                let owner = state.clone();
                match accepted.channel() {
                    Channel::Team => {
                        let Some(reservation)=state.connections.reserve(state.endpoint.id(),accepted.remote_id(),false) else {drop(accepted);continue;};
                        let send = sessions.clone();
                        tasks.spawn(async move {
                            let options = SessionOptions {profile:owner.settings.profile.clone(),deadline:DEADLINE,cancel:reservation.cancel()};
                            let result=tokio::select! {
                                biased;
                                _=stopped(owner.stop.subscribe())=>return id,
                                result=PeerSession::accept_connection(&owner.endpoint,accepted,owner.authority.clone(),options)=>result,
                            };
                            if let Ok(session) = result {
                                tokio::select! {
                                    biased;
                                    _ = stopped(owner.stop.subscribe()) => {},
                                    _ = send.send(connections::Ready{session,reservation}) => {},
                                }
                            }
                            id
                        });
                    }
                    Channel::Join => {
                        let connection = owner.endpoint.into_connection(accepted).map_err(|_| ControlError::Transport)?;
                        let fence = Arc::new(AdmissionFence::new(stop.clone()));
                        pending.insert(id,(connection.clone(),fence.clone()));
                        tasks.spawn(async move {
                            let authority = owner.authority.clone();
                            let _ = serve_join(connection,&owner.settings.profile,DEADLINE,move |peer,request| async move {
                                authority.admit_join_fenced(peer,request,fence).await.map_err(|_| Refusal::Denied)
                            }).await;
                            id
                        });
                    }
                }
            }
        }
    }
    for (connection, fence) in pending.values() {
        if fence.stop_pending() {
            connection.close(0u32.into(), b"host stopped");
        }
    }
    while tasks.join_next().await.is_some() {}
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
