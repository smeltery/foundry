//! Synchronous durable work stays off the reactor under current team authority.
use super::*;
use crate::{
    daemon_lease::DaemonLease,
    daemon_outbox::{OutboxLedger, QueueAction, Settlement},
    mail_store::{MailStore, TransportDisposition},
    peer_session::RoutingView,
};
use std::sync::Mutex;

pub(super) struct Outbox {
    inner: Arc<Mutex<Store>>,
}
struct Store {
    mail: MailStore,
    ledgers: BTreeMap<(EndpointId, u64), OutboxLedger>,
    _lease: DaemonLease,
}
impl Outbox {
    pub async fn open(state: &State) -> Result<Self, ControlError> {
        let root = state.root.clone();
        let lease = state.engine.lease().map_err(|_| ControlError::Storage)?;
        tokio::task::spawn_blocking(move || {
            let mut mail = MailStore::open(&root).map_err(|_| ControlError::Storage)?;
            mail.recover_transport()
                .map_err(|_| ControlError::Storage)?;
            Ok(Self {
                inner: Arc::new(Mutex::new(Store {
                    mail,
                    ledgers: BTreeMap::new(),
                    _lease: lease,
                })),
            })
        })
        .await
        .map_err(|_| ControlError::Storage)?
    }
    async fn with<T: Send + 'static>(
        &self,
        view: Option<RoutingView>,
        operation: impl FnOnce(&mut Store) -> Result<T, ControlError> + Send + 'static,
    ) -> Result<T, ControlError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _view = view;
            {
                let mut store = inner.lock().map_err(|_| ControlError::Storage)?;
                operation(&mut store)
            }
        })
        .await
        .map_err(|_| ControlError::Storage)?
    }
    pub async fn register(&self, peer: EndpointId, generation: u64) -> Result<(), ControlError> {
        self.with(None, move |store| {
            let ledger = OutboxLedger::new(peer, generation, crate::peer_session::MAX_PENDING)
                .map_err(|_| ControlError::Storage)?;
            if store.ledgers.insert((peer, generation), ledger).is_some() {
                return Err(ControlError::Storage);
            }
            Ok(())
        })
        .await
    }
    pub async fn batch(
        &self,
        view: RoutingView,
        connections: Vec<(EndpointId, u64)>,
    ) -> Result<Batch, ControlError> {
        let team = view.team.clone();
        let active = view
            .members
            .iter()
            .filter(|m| m.active && m.endpoint != view.local)
            .map(|m| m.endpoint)
            .collect::<std::collections::BTreeSet<_>>();
        self.with(Some(view), move |store| {
            let mut batch = Batch {
                retire: Vec::new(),
                messages: Vec::new(),
            };
            // One current authority guard covers the complete global scan AND
            // all attempt claims. Concurrent Engine enqueues use this same gate.
            for (key, ledger) in &mut store.ledgers {
                if !active.contains(&key.0)
                    || ledger.receipt_deadline_elapsed(std::time::Instant::now())
                    || ledger
                        .requires_retirement(&store.mail)
                        .map_err(|_| ControlError::Storage)?
                {
                    batch.retire.push(*key);
                }
            }
            if !batch.retire.is_empty() {
                return Ok(batch);
            }
            let checked = now().map_err(|_| ControlError::Storage)?;
            for (peer, generation) in connections {
                let ledger = store
                    .ledgers
                    .get_mut(&(peer, generation))
                    .ok_or(ControlError::Storage)?;
                for queued in store
                    .mail
                    .transport_for(&team, peer, checked)
                    .map_err(|_| ControlError::Storage)?
                {
                    match ledger
                        .prepare(&mut store.mail, &queued)
                        .map_err(|_| ControlError::Storage)?
                    {
                        QueueAction::Pending => {}
                        QueueAction::Retire => {
                            // A history-capacity retirement discovered during
                            // preparation also fences claims already staged in
                            // this batch. No ID owner is reset on a live stream.
                            batch.retire = store.ledgers.keys().copied().collect();
                            batch.messages.clear();
                            return Ok(batch);
                        }
                        QueueAction::Send(message) => {
                            batch.messages.push((peer, generation, message));
                            break;
                        }
                    }
                }
            }
            Ok(batch)
        })
        .await
    }
    pub async fn receipt(
        &self,
        view: RoutingView,
        peer: EndpointId,
        generation: u64,
        id: String,
        disposition: TransportDisposition,
    ) -> Result<Settlement, ControlError> {
        if !view.members.iter().any(|m| m.active && m.endpoint == peer) {
            return Ok(Settlement::Retire);
        }
        self.with(Some(view), move |store| {
            let ledger = store
                .ledgers
                .get_mut(&(peer, generation))
                .ok_or(ControlError::Storage)?;
            ledger
                .settle(&mut store.mail, &id, disposition)
                .map_err(|_| ControlError::Storage)
        })
        .await
    }
    pub async fn disconnected(
        &self,
        peer: EndpointId,
        generation: u64,
    ) -> Result<(), ControlError> {
        self.with(None, move |store| {
            let ledger = store
                .ledgers
                .remove(&(peer, generation))
                .ok_or(ControlError::Storage)?;
            ledger
                .disconnected(&mut store.mail)
                .map_err(|_| ControlError::Storage)
        })
        .await
    }
}

pub(super) struct Batch {
    pub retire: Vec<(EndpointId, u64)>,
    pub messages: Vec<(EndpointId, u64, Box<crate::wire::Message>)>,
}
