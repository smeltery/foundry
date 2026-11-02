//! Synchronous durable work stays off the reactor under current team authority.
use super::*;
use crate::{
    daemon_lease::DaemonLease,
    daemon_outbox::{OutboxLedger, QueueAction, Settlement},
    mail_store::MailStore,
    peer_session::{ReceiptPublisher, RoutingView},
};
use std::sync::Mutex;
#[path = "outbox_receipts.rs"]
mod receipts;

pub(super) struct Outbox {
    inner: Arc<Mutex<Store>>,
}
struct Store {
    mail: MailStore,
    ledgers: BTreeMap<(EndpointId, u64), OutboxLedger>,
    receipts: BTreeMap<(EndpointId, u64), Arc<Mutex<receipts::Slot>>>,
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
                    receipts: BTreeMap::new(),
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
                let result = operation(&mut store);
                if result.is_err() {
                    store.close_receipts()?;
                }
                result
            }
        })
        .await
        .map_err(|_| ControlError::Storage)?
    }
    pub async fn register(
        &self,
        peer: EndpointId,
        generation: u64,
    ) -> Result<ReceiptPublisher, ControlError> {
        self.with(None, move |store| {
            let ledger = OutboxLedger::new(peer, generation, crate::peer_session::MAX_PENDING)
                .map_err(|_| ControlError::Storage)?;
            if store.ledgers.insert((peer, generation), ledger).is_some() {
                return Err(ControlError::Storage);
            }
            let slot = Arc::new(Mutex::new(receipts::Slot::default()));
            if store
                .receipts
                .insert((peer, generation), slot.clone())
                .is_some()
            {
                return Err(ControlError::Storage);
            }
            Ok(receipts::publisher(slot))
        })
        .await
    }
    pub async fn batch(
        &self,
        view: RoutingView,
        connections: Vec<(EndpointId, u64)>,
    ) -> Result<Batch, ControlError> {
        let team = view.team.clone();
        let revision = view.revision;
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
            for key in store.ledgers.keys().copied().collect::<Vec<_>>() {
                let retire = store.drain(key, revision, active.contains(&key.0))?;
                let ledger = store.ledgers.get_mut(&key).ok_or(ControlError::Storage)?;
                if retire
                    || ledger.receipt_deadline_elapsed(std::time::Instant::now())
                    || ledger
                        .requires_retirement(&store.mail)
                        .map_err(|_| ControlError::Storage)?
                {
                    batch.retire.push(key);
                }
            }
            if !batch.retire.is_empty() {
                store.close_selected(&batch.retire)?;
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
                            store.close_selected(&batch.retire)?;
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
    pub async fn drain_receipts(
        &self,
        view: RoutingView,
        peer: EndpointId,
        generation: u64,
    ) -> Result<bool, ControlError> {
        let active = view.members.iter().any(|m| m.active && m.endpoint == peer);
        let revision = view.revision;
        self.with(Some(view), move |store| {
            store.drain((peer, generation), revision, active)
        })
        .await
    }
    pub async fn close_receipts(
        &self,
        view: Option<RoutingView>,
        peer: EndpointId,
        generation: u64,
    ) -> Result<(), ControlError> {
        let active = view
            .as_ref()
            .is_some_and(|v| v.members.iter().any(|m| m.active && m.endpoint == peer));
        let revision = view.as_ref().map_or(0, |v| v.revision);
        self.with(view, move |store| {
            store.drain((peer, generation), revision, active)?;
            store.close_selected(&[(peer, generation)])
        })
        .await
    }
    pub async fn disconnected(
        &self,
        peer: EndpointId,
        generation: u64,
    ) -> Result<(), ControlError> {
        self.with(None, move |store| {
            store.close_selected(&[(peer, generation)])?;
            store
                .receipts
                .remove(&(peer, generation))
                .ok_or(ControlError::Storage)?;
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
