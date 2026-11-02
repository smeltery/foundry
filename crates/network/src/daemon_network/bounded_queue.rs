use super::{Event, Live};
use crate::{
    peer_session::{DriverEvent, PeerEvent},
    wire::Presence,
};
use iroh::EndpointId;
use std::collections::BTreeMap;
use tokio::sync::{mpsc, watch};

pub(in crate::daemon_network) const EVENT_BUDGET: usize = 32;

// Presence is a replaceable snapshot, not a receipt or a lifecycle transition.
// Invalid attribution remains in the FIFO so the normal handler closes it.
pub(in crate::daemon_network) fn coalesce(
    peer: EndpointId,
    event: Option<DriverEvent>,
    latest: &watch::Sender<Option<Presence>>,
) -> Result<Option<DriverEvent>, ()> {
    match event {
        Some(DriverEvent::Peer(PeerEvent::Presence {
            peer: actual,
            presence,
        })) if actual == peer => {
            latest.send_replace(Some(presence));
            Err(())
        }
        event => Ok(event),
    }
}

// Snapshot once: new arrivals cannot make this turn's work unbounded.
pub(in crate::daemon_network) fn ready(
    received: &mut mpsc::Receiver<Event>,
    selected: Option<Event>,
) -> Vec<Event> {
    let mut events = Vec::with_capacity(EVENT_BUDGET);
    if let Some(event) = selected {
        events.push(event);
    }
    let count = received.len().min(EVENT_BUDGET - events.len());
    for _ in 0..count {
        match received.try_recv() {
            Ok(event) => events.push(event),
            Err(_) => break,
        }
    }
    events
}

pub(in crate::daemon_network) fn has_presence(live: &BTreeMap<EndpointId, Live>) -> bool {
    live.values()
        .any(|connection| connection.received_presence.has_changed().unwrap_or(false))
}

// Each slot belongs to Live, so retirement cannot transfer it to a new generation.
// One round-robin projection bounds durable presence work between expiry scans.
pub(in crate::daemon_network) fn presence(
    live: &mut BTreeMap<EndpointId, Live>,
    cursor: &mut Option<EndpointId>,
) -> Option<Event> {
    let pending = || {
        live.iter()
            .filter(|(_, connection)| connection.received_presence.has_changed().unwrap_or(false))
            .map(|(peer, _)| *peer)
    };
    let peer = pending()
        .find(|peer| cursor.is_none_or(|last| *peer > last))
        .or_else(|| pending().next())?;
    let connection = live.get_mut(&peer)?;
    let presence = connection.received_presence.borrow_and_update().clone()?;
    *cursor = Some(peer);
    Some(Event {
        peer,
        generation: connection.generation,
        event: Some(DriverEvent::Peer(PeerEvent::Presence { peer, presence })),
    })
}

#[cfg(test)]
#[path = "bounded_queue_tests.rs"]
mod tests;
