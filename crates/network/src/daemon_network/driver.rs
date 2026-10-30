use super::*;
use crate::peer_session::{DriverEvent, PeerDriver, PeerSender};
use tokio::{sync::mpsc, task::JoinHandle};
pub(super) struct Live {
    pub generation: u64,
    pub outgoing: bool,
    _reservation: connections::Reservation,
    pub published: Option<u64>,
    pub presence: Option<crate::wire::Presence>,
    pub stop: watch::Sender<bool>,
    pub sender: PeerSender,
    pub pump: JoinHandle<crate::peer_session::Result<()>>,
}
pub(super) struct Event {
    pub peer: EndpointId,
    pub generation: u64,
    pub event: Option<DriverEvent>,
}
pub(super) async fn retire(
    state: &State,
    outbox: &outbox::Outbox,
    live: Live,
    peer: EndpointId,
) -> Result<(), ControlError> {
    live.stop.send_replace(true);
    live.sender.stop();
    // The pump owns shutdown, which joins the sole writer and persistent reader.
    let joined = live.pump.await.map_err(|_| ControlError::Storage)?;
    outbox.disconnected(peer, live.generation).await?;
    state
        .engine
        .with(move |engine| engine.remote_disconnected(peer, live.generation))
        .await
        .map_err(|_| ControlError::Storage)?;
    // Transport errors trigger retirement; the queue stays durable.
    let _ = joined;
    Ok(())
}
pub(super) async fn install(
    state: &State,
    outbox: &outbox::Outbox,
    ready: connections::Ready,
    generation: u64,
    outgoing: bool,
    events: mpsc::Sender<Event>,
) -> Result<Live, ControlError> {
    let connections::Ready {
        session,
        reservation,
    } = ready;
    let peer = session.peer();
    let (sender, mut driver) = PeerDriver::start(session).map_err(|_| ControlError::Transport)?;
    outbox.register(peer, generation).await?;
    let stop = state.stop.subscribe();
    let (retirement, retiring) = watch::channel(false);
    let pump = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                _ = stopped(stop.clone()) => break,
                _ = stopped(retiring.clone()) => break,
                event = driver.next_event() => event,
            };
            let end = event.is_none();
            // A stopping driver can always drain even if the runtime mailbox is full.
            tokio::select! {
                biased;
                _ = stopped(stop.clone()) => break,
                _ = stopped(retiring.clone()) => break,
                sent = events.send(Event {peer,generation,event}) => if sent.is_err() {break;},
            }
            if end {
                break;
            }
        }
        driver.shutdown().await
    });
    Ok(Live {
        generation,
        outgoing,
        _reservation: reservation,
        published: None,
        presence: None,
        stop: retirement,
        sender,
        pump,
    })
}

// Success accepts an admitted address for supervision; it does not confirm a handshake.
// Existing live and pending connections retain their ownership and generation.
pub(super) async fn connect(state: &State, address: EndpointAddr) -> Result<(), ControlError> {
    let peer = address.id;
    let view = state
        .authority
        .routing_view()
        .await
        .map_err(|_| ControlError::Rejected)?;
    if !view
        .members
        .iter()
        .any(|m| m.active && m.endpoint == peer && peer != view.local)
    {
        return Err(ControlError::Rejected);
    }
    state.addresses.write().await.insert(peer, address);
    drop(view);
    Ok(())
}
