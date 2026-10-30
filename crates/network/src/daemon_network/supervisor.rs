use super::{
    driver::{Event, Live, install, retire},
    *,
};
use crate::peer_session::{PeerSession, SessionOptions};
use std::collections::BTreeSet;
use tokio::{sync::mpsc, task::JoinSet};

pub(super) struct Connect {
    pub address: EndpointAddr,
    pub reply: tokio::sync::oneshot::Sender<Result<(), ControlError>>,
}
pub(super) async fn run(
    state: Arc<State>,
    mut commands: mpsc::Receiver<Connect>,
) -> Result<(), ControlError> {
    let outbox = outbox::Outbox::open(&state).await?;
    let (incoming, mut sessions) = mpsc::channel(MAX_CONNECTING);
    let router_state = state.clone();
    let routing = tokio::spawn(async move {
        let result = router::run(router_state.clone(), incoming).await;
        if result.is_err() {
            router_state.stop.send_replace(true);
        }
        result
    });
    let (events, mut received) = mpsc::channel::<Event>(32);
    let mut live = BTreeMap::<EndpointId, Live>::new();
    let mut connecting =
        JoinSet::<(EndpointId, crate::peer_session::Result<connections::Ready>)>::new();
    let mut pending = BTreeSet::new();
    let mut generation = 0u64;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = async {
        loop {
            if *state.stop.borrow() { break; }
            tokio::select! {
                biased;
                _ = stopped(state.stop.subscribe()) => break,
                command = commands.recv() => {
                    if let Some(Connect { address, reply }) = command {
                        let result = driver::connect(&state, address).await;
                        let _ = reply.send(result);
                    }
                }
                event = received.recv() => {
                    if let Some(event) = event {
                        events::handle(&state, &outbox, &mut live, event).await?;
                    }
                }
                session = sessions.recv() => {
                    if let Some(session) = session {
                        if *session.reservation.cancel().borrow() { continue; }
                        let peer = session.session.peer();
                        let preferred = state.endpoint.id() > peer;
                        if live.get(&peer).is_none_or(|old| preferred && old.outgoing) {
                            if let Some(old) = live.remove(&peer) {
                                retire(&state, &outbox, old, peer).await?;
                            }
                            generation = generation.checked_add(1).ok_or(ControlError::Busy)?;
                            live.insert(peer, install(&state, &outbox, session, generation, false, events.clone()).await?);
                        }
                    }
                }
                finished = connecting.join_next(), if !connecting.is_empty() => {
                    match finished {
                        Some(Ok((peer, session))) => {
                            pending.remove(&peer);
                            if let Ok(session) = session {
                                if *session.reservation.cancel().borrow() { continue; }
                                let preferred = state.endpoint.id() < peer;
                                if live.get(&peer).is_none_or(|old| preferred && !old.outgoing) {
                                    if let Some(old) = live.remove(&peer) {
                                        retire(&state, &outbox, old, peer).await?;
                                    }
                                    generation = generation.checked_add(1).ok_or(ControlError::Busy)?;
                                    live.insert(peer, install(&state, &outbox, session, generation, true, events.clone()).await?);
                                }
                            }
                        }
                        _ => return Err(ControlError::Storage),
                    }
                }
                _ = state.wake.notified() => {}
                _ = interval.tick() => {}
            }
            let members = dispatch::batch(&state, &outbox, &mut live).await?;
            for peer in members {
                if live.contains_key(&peer) || pending.contains(&peer) || connecting.len() >= MAX_CONNECTING {
                    continue;
                }
                let address = state.addresses.read().await.get(&peer).cloned().unwrap_or_else(|| EndpointAddr::new(peer));
                let Some(reservation) = state.connections.outgoing(state.endpoint.id(), peer) else {
                    continue;
                };
                let owner = state.clone();
                pending.insert(peer);
                connecting.spawn(async move {
                    let options = SessionOptions {
                        profile: owner.settings.profile.clone(),
                        deadline: DEADLINE,
                        cancel: reservation.cancel(),
                    };
                    let result = tokio::select! {
                        biased;
                        _ = stopped(owner.stop.subscribe()) => Err(crate::peer_session::PeerError::Closed),
                        result = PeerSession::connect(&owner.endpoint, address, owner.authority.clone(), options) => result,
                    };
                    (peer, result.map(|session| connections::Ready { session, reservation }))
                });
            }
        }
        Ok(())
    }.await;
    state.stop.send_replace(true);
    state.connections.stop();
    connecting.abort_all();
    while connecting.join_next().await.is_some() {}
    let mut cleanup = Ok(());
    for (peer, connection) in live {
        if let Err(error) = retire(&state, &outbox, connection, peer).await {
            cleanup = Err(error);
        }
    }
    let code_cleanup = state.codes.shutdown().await;
    let routed = routing.await.map_err(|_| ControlError::Storage)?;
    result.and(cleanup).and(code_cleanup).and(routed)
}
