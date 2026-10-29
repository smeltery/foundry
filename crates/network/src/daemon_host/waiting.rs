use super::*;
use crate::daemon_engine::{Auth, InboxWait, MAX_INBOX_REPLY_BYTES, MAX_WAIT_SECS, Outcome, Reply};
use response::{PreparedReply, Response};
use tokio::sync::OwnedSemaphorePermit;
struct WaitOwner(Arc<InboxWait>);
impl Drop for WaitOwner {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
#[path = "waiting_tests.rs"]
mod tests;
pub(super) async fn prepare(
    engine: Arc<OwnedEngine>,
    connection: ConnectionId,
    auth: Auth,
    fence: &cancellation::ConnectionFence,
    positive: bool,
) -> Result<Arc<InboxWait>, EngineError> {
    let registry = fence.registry();
    // The captured engine retains its actual lease even if this future is aborted.
    tokio::task::spawn_blocking(move || {
        let mut guard = engine
            .inner
            .lock()
            .map_err(|_| EngineError::StorageUncertain)?;
        let wait = Arc::new(if positive {
            guard.begin_inbox_wait(connection, &auth)?
        } else {
            guard.begin_inbox_read(connection, &auth)?
        });
        registry.register(connection, &wait);
        Ok(wait)
    })
    .await
    .map_err(|_| EngineError::StorageUncertain)?
}
fn empty(id: u64) -> Response {
    Response::Small(Reply {
        id,
        result: ReplyResult::Ok(Outcome::Inbox {
            messages: Vec::new(),
            text: String::new(),
        }),
    })
}
async fn acquire(
    engine: &OwnedEngine,
    wait: &InboxWait,
) -> Result<Option<OwnedSemaphorePermit>, EngineError> {
    let changed = wait.arrival_notification();
    let acquire = engine.replies.clone().acquire_owned();
    tokio::pin!(acquire);
    let work = async {
        loop {
            let notification = changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if wait.cancelled() {
                return Ok(None);
            }
            tokio::select! {
                biased;
                permit = &mut acquire => {
                    return permit.map(Some).map_err(|_| EngineError::Output);
                }
                _ = notification => {}
            }
        }
    };
    tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .map_err(|_| EngineError::Limit)?
}
async fn prepare_output(
    engine: Arc<OwnedEngine>,
    wait: Arc<InboxWait>,
    id: u64,
    peek: bool,
    slot: OwnedSemaphorePermit,
) -> Result<(Response, bool), HostError> {
    tokio::task::spawn_blocking(move || {
        // The closure owns both slot and actual daemon lease even after its
        // async waiter is aborted. Only committed, sealed bytes leave it.
        let mut output = match spool::Spool::create(&engine.root, MAX_INBOX_REPLY_BYTES) {
            Ok(output) => output,
            Err(_) => {
                return Ok((
                    Response::Small(Reply {
                        id,
                        result: ReplyResult::Error(EngineError::Output),
                    }),
                    false,
                ));
            }
        };
        let result = engine
            .inner
            .lock()
            .map_err(|_| HostError::Storage)?
            .poll_inbox_wait_to_output(&wait, id, peek, &mut output);
        match result {
            Ok(Some(summary)) => {
                let empty = summary.messages == 0;
                let prepared = PreparedReply::new(output, summary, slot, engine)
                    .map_err(|_| HostError::Storage)?;
                Ok((Response::Inbox(prepared), empty))
            }
            Ok(None) => Ok((empty(id), false)),
            Err(error) => Ok((
                Response::Small(Reply {
                    id,
                    result: ReplyResult::Error(error),
                }),
                false,
            )),
        }
    })
    .await
    .map_err(|_| HostError::Task)?
}
pub(super) async fn poll(
    engine: Arc<OwnedEngine>,
    wait: Arc<InboxWait>,
    id: u64,
    peek: bool,
    seconds: u64,
) -> Result<Response, HostError> {
    let owner = WaitOwner(wait);
    let wait = &owner.0;
    let changed = wait.arrival_notification();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds.min(MAX_WAIT_SECS));
    loop {
        let arrived = changed.notified();
        tokio::pin!(arrived);
        arrived.as_mut().enable();
        let slot = match acquire(&engine, wait).await {
            Ok(Some(slot)) => slot,
            Ok(None) => return Ok(empty(id)),
            Err(error) => {
                return Ok(Response::Small(Reply {
                    id,
                    result: ReplyResult::Error(error),
                }));
            }
        };
        let (reply, is_empty) =
            prepare_output(engine.clone(), wait.clone(), id, peek, slot).await?;
        if wait.cancelled() || !is_empty || tokio::time::Instant::now() >= deadline {
            return Ok(reply);
        }
        // Empty polls own no disk/slot while waiting, and do not prevent an
        // ordinary command or another ready inbox from making progress.
        drop(reply);
        let _ = tokio::time::timeout_at(deadline, arrived).await;
    }
}
