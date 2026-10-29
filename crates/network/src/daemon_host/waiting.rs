use super::*;
use crate::daemon_engine::{Auth, InboxWait, MAX_WAIT_SECS, Outcome, Reply};
struct WaitOwner(Arc<InboxWait>);
impl Drop for WaitOwner {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
pub(super) async fn prepare(
    engine: Arc<OwnedEngine>,
    connection: ConnectionId,
    auth: Auth,
    fence: &cancellation::ConnectionFence,
) -> Result<Arc<InboxWait>, EngineError> {
    let registry = fence.registry();
    // The captured engine retains its actual lease even if this future is aborted.
    tokio::task::spawn_blocking(move || {
        let wait = Arc::new(
            engine
                .inner
                .lock()
                .map_err(|_| EngineError::StorageUncertain)?
                .begin_inbox_wait(connection, &auth)?,
        );
        registry.register(connection, &wait);
        Ok(wait)
    })
    .await
    .map_err(|_| EngineError::StorageUncertain)?
}
pub(super) async fn poll(
    engine: Arc<OwnedEngine>,
    wait: Arc<InboxWait>,
    id: u64,
    peek: bool,
    seconds: u64,
) -> Result<Reply, HostError> {
    let owner = WaitOwner(wait);
    let wait = &owner.0;
    let changed = wait.arrival_notification();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds.min(MAX_WAIT_SECS));
    loop {
        // Enable before touching durable state: arrival between check and await
        // remains observable. No engine/SQLite guard survives into the await.
        let arrived = changed.notified();
        tokio::pin!(arrived);
        arrived.as_mut().enable();
        let owned = engine.clone();
        let token = wait.clone();
        let reply = tokio::task::spawn_blocking(move || {
            Ok::<_, HostError>(
                owned
                    .inner
                    .lock()
                    .map_err(|_| HostError::Storage)?
                    .poll_inbox_wait(&token, id, peek),
            )
        })
        .await
        .map_err(|_| HostError::Task)??;
        if wait.cancelled()
            || !matches!(&reply.result, ReplyResult::Ok(Outcome::Inbox { messages, .. }) if messages.is_empty())
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(reply);
        }
        let _ = tokio::time::timeout_at(deadline, arrived).await;
    }
}
