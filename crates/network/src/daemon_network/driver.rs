#[path = "bounded_queue.rs"]
pub(super) mod bounded_queue;
use super::*;
use crate::peer_session::{DriverEvent, PeerDriver, PeerSender};
use tokio::{sync::mpsc, task::JoinHandle};
pub(super) struct Live {
    pub generation: u64,
    pub outgoing: bool,
    pub received_presence: watch::Receiver<Option<crate::wire::Presence>>,
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
    mut live: Live,
    peer: EndpointId,
) -> Result<(), ControlError> {
    // Close transport before waiting for authority held by an in-flight writer.
    // Accepted receipt work stays registered until its guarded drain below.
    live.stop.send_replace(true);
    live.sender.stop();
    let receipts = match state.view().await {
        Ok(view) => outbox.close_receipts(view, peer, live.generation).await,
        Err(error) => {
            let _ = outbox.close_receipts(None, peer, live.generation).await;
            Err(error)
        }
    };
    // Always stop and join before resetting the exact generation, including
    // when receipt closure failed. Transport errors leave the queue durable.
    let joined = live.pump.await.map_err(|_| ControlError::Storage);
    let disconnected = outbox.disconnected(peer, live.generation).await;
    // Read even a closed watch: it may retain an unprojected final snapshot.
    let presence = live.received_presence.borrow_and_update().clone();
    let projection = events::retired_presence(state, peer, live.generation, presence).await;
    receipts
        .and(joined.map(|_| ()))
        .and(disconnected)
        .and(projection)
}

pub(super) async fn install(
    state: &Arc<State>,
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
    let publisher = outbox.register(peer, generation).await?;
    let (sender, mut driver) =
        PeerDriver::start_receipted(session, publisher).map_err(|_| ControlError::Transport)?;
    let stop = state.stop.subscribe();
    let (retirement, retiring) = watch::channel(false);
    let (latest, received_presence) = watch::channel(None);
    let owner = state.clone();
    let pump = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                _ = stopped(stop.clone()) => break,
                _ = stopped(retiring.clone()) => break,
                event = driver.next_event() => event,
            };
            let event = match bounded_queue::coalesce(peer, event, &latest) {
                Ok(event) => event,
                Err(()) => {
                    owner.wake.notify_one();
                    continue;
                }
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
        received_presence,
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
