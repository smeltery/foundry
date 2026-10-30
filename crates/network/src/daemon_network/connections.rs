//! Select one transport before a handshake can hold the team authority gate.
use super::*;
use crate::peer_session::PeerSession;
use std::sync::Mutex;
struct Slot {
    outgoing: bool,
    stop: watch::Sender<bool>,
}
#[derive(Default)]
pub(super) struct Connections(Arc<Mutex<BTreeMap<EndpointId, Arc<Slot>>>>);
pub(super) struct Reservation {
    peer: EndpointId,
    slot: Arc<Slot>,
    registry: Arc<Mutex<BTreeMap<EndpointId, Arc<Slot>>>>,
}
impl Reservation {
    pub fn cancel(&self) -> watch::Receiver<bool> {
        self.slot.stop.subscribe()
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.slot.stop.send_replace(true);
        let mut slots = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        if slots
            .get(&self.peer)
            .is_some_and(|slot| Arc::ptr_eq(slot, &self.slot))
        {
            slots.remove(&self.peer);
        }
    }
}
pub(super) struct Ready {
    pub session: PeerSession,
    pub reservation: Reservation,
}
impl Connections {
    pub fn reserve(
        &self,
        local: EndpointId,
        peer: EndpointId,
        outgoing: bool,
    ) -> Option<Reservation> {
        if local == peer {
            return None;
        }
        let mut slots = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = slots.get(&peer) {
            let preferred = (local < peer) == outgoing;
            if old.outgoing == outgoing || !preferred {
                return None;
            }
            old.stop.send_replace(true);
        }
        let (stop, _) = watch::channel(false);
        let slot = Arc::new(Slot { outgoing, stop });
        slots.insert(peer, slot.clone());
        Some(Reservation {
            peer,
            slot,
            registry: self.0.clone(),
        })
    }
    pub fn stop(&self) {
        for slot in self.0.lock().unwrap_or_else(|p| p.into_inner()).values() {
            slot.stop.send_replace(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (EndpointId, EndpointId) {
        let a = iroh::SecretKey::generate().public();
        let b = iroh::SecretKey::generate().public();
        (a.min(b), a.max(b))
    }
    #[test]
    fn preferred_dial_replaces_incoming_before_handshake_and_old_drop_cannot_erase_it() {
        let (a, b) = pair();
        let registry = Connections::default();
        let incoming = registry.reserve(a, b, false).unwrap();
        let old_cancel = incoming.cancel();
        let outgoing = registry.reserve(a, b, true).unwrap();
        assert!(*old_cancel.borrow());
        drop(incoming);
        assert!(registry.reserve(a, b, false).is_none());
        assert!(!*outgoing.cancel().borrow());
        drop(outgoing);
        assert!(registry.reserve(a, b, false).is_some());
    }
    #[test]
    fn sole_nonpreferred_connection_is_allowed_and_shutdown_latches_before_subscribe() {
        let (a, b) = pair();
        let registry = Connections::default();
        let outgoing = registry.reserve(b, a, true).unwrap();
        let old = outgoing.cancel();
        let incoming = registry.reserve(b, a, false).unwrap();
        assert!(*old.borrow());
        registry.stop();
        assert!(*incoming.cancel().borrow());
    }
}
