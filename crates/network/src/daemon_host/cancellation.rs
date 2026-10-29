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
}
impl WaitRegistry {
    pub fn connection(self: &Arc<Self>, id: ConnectionId) -> ConnectionFence {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .connections
            .insert(id, Vec::new());
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
            if waits.len() < 8 {
                waits.push(Arc::downgrade(wait));
                return;
            }
        }
        wait.cancel();
    }
    pub fn cancel_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        for waits in state.connections.values() {
            cancel(waits);
        }
    }
    fn cancel_connection(&self, id: ConnectionId) {
        if let Some(waits) = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .connections
            .remove(&id)
        {
            cancel(&waits);
        }
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
