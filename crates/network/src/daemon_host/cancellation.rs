use super::*;
use crate::daemon_engine::InboxWait;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::Weak,
    task::{Context, Poll},
};
/// Host-specific cancellation ownership. No Engine mutex or SQLite access occurs
/// while closing gates, so blocked database work cannot delay the fence.
#[derive(Default)]
pub(super) struct WaitRegistry {
    state: Mutex<RegistryState>,
}
#[derive(Default)]
struct RegistryState {
    closed: bool,
    connections: BTreeMap<ConnectionId, Vec<Weak<InboxWait>>>,
    signals: BTreeMap<ConnectionId, watch::Sender<bool>>,
}
impl WaitRegistry {
    pub fn connection(self: &Arc<Self>, id: ConnectionId) -> ConnectionFence {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (signal, _) = watch::channel(state.closed);
        state.signals.insert(id, signal);
        state.connections.insert(id, Vec::new());
        drop(state);
        ConnectionFence {
            registry: self.clone(),
            id,
        }
    }
    pub fn register(&self, id: ConnectionId, wait: &Arc<InboxWait>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.closed
            && let Some(waits) = state.connections.get_mut(&id)
        {
            waits.retain(|w| w.upgrade().is_some_and(|w| !w.cancelled()));
            // Eight positive waits plus the connection's one immediate read.
            if waits.len() < 9 {
                waits.push(Arc::downgrade(wait));
                return;
            }
        }
        wait.cancel();
    }
    pub fn cancel_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        for signal in state.signals.values() {
            signal.send_replace(true);
        }
        for waits in state.connections.values() {
            cancel(waits);
        }
    }
    fn cancel_connection(&self, id: ConnectionId) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(signal) = state.signals.remove(&id) {
            signal.send_replace(true);
        }
        if let Some(waits) = state.connections.remove(&id) {
            cancel(&waits);
        }
    }
    fn signal(&self, id: ConnectionId) -> watch::Receiver<bool> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(signal) = state.signals.get(&id) {
            return signal.subscribe();
        }
        let (_, receiver) = watch::channel(true);
        receiver
    }
}
fn cancel(waits: &[Weak<InboxWait>]) {
    for wait in waits.iter().filter_map(Weak::upgrade) {
        wait.cancel();
    }
}
pub(super) struct ConnectionFence {
    registry: Arc<WaitRegistry>,
    id: ConnectionId,
}
impl ConnectionFence {
    pub fn signal(&self) -> watch::Receiver<bool> {
        self.registry.signal(self.id)
    }
    pub fn reader_guard(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            id: self.id,
        }
    }
    pub fn registry(&self) -> Arc<WaitRegistry> {
        self.registry.clone()
    }
    pub fn cancel(&self) {
        self.registry.cancel_connection(self.id);
    }
}
impl Drop for ConnectionFence {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub(super) struct RootFence(pub Arc<WaitRegistry>);
impl Drop for RootFence {
    fn drop(&mut self) {
        self.0.cancel_all();
    }
}
/// Explicit abort publishes cancellation before asking Tokio to abort the parent.
/// Merely calling a raw Tokio JoinHandle's abort cannot promise synchronous Drop.
pub struct DaemonTask {
    pub(super) task: tokio::task::JoinHandle<Result<(), HostError>>,
    pub(super) registry: Arc<WaitRegistry>,
}
impl DaemonTask {
    pub fn abort(&self) {
        self.registry.cancel_all();
        self.task.abort();
    }
}
impl Drop for DaemonTask {
    fn drop(&mut self) {
        self.abort();
    }
}
impl Future for DaemonTask {
    type Output = Result<Result<(), HostError>, tokio::task::JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.task).poll(cx)
    }
}

#[cfg(test)]
#[path = "disconnect_tests.rs"]
mod disconnect_tests;
