use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
pub(super) struct WaitState {
    generation: AtomicU64,
    active: AtomicUsize,
}
/// Host-owned polling guard, never deserialized or logged. Dropping it restores
/// push eligibility; waiting holds neither the engine mutex nor a DB transaction.
pub struct InboxWait {
    connection: ConnectionId,
    auth: Auth,
    generation: u64,
    closed: AtomicBool,
    state: Arc<WaitState>,
    changed: Arc<Notify>,
}
impl InboxWait {
    pub fn arrival_notification(&self) -> Arc<Notify> {
        self.changed.clone()
    }
    pub fn cancel(&self) {
        self.closed.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
    pub fn cancelled(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            || self.state.generation.load(Ordering::Acquire) != self.generation
    }
}
impl Drop for InboxWait {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::Release);
        self.changed.notify_waiters();
    }
}
impl Engine {
    pub fn arrival_notification(&self) -> Arc<Notify> {
        self.changed.clone()
    }
    /// Installed push adapters must check this gate under the engine lock before
    /// claiming delivery. This engine currently advertises only Pull capability.
    pub fn is_polling(&self, conversation: &Conversation) -> bool {
        self.waits
            .get(&mailbox_address(conversation))
            .is_some_and(|state| state.active.load(Ordering::Acquire) > 0)
    }
    pub fn begin_inbox_wait(
        &mut self,
        connection: ConnectionId,
        auth: &Auth,
    ) -> Result<InboxWait, EngineError> {
        self.authorize(connection, auth)?;
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        let state = self.wait_state(auth);
        state
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_ATTEMPTS).then_some(n + 1)
            })
            .map_err(|_| EngineError::Limit)?;
        Ok(InboxWait {
            closed: AtomicBool::new(false),
            connection,
            auth: auth.clone(),
            generation: state.generation.load(Ordering::Acquire),
            state,
            changed: self.changed.clone(),
        })
    }
    pub fn poll_inbox_wait(&mut self, wait: &InboxWait, id: u64, peek: bool) -> Reply {
        let result = if self.uncertain {
            Err(EngineError::StorageUncertain)
        } else if let Err(error) = self.authorize(wait.connection, &wait.auth) {
            Err(error)
        } else if wait.cancelled() {
            Ok(Outcome::Inbox {
                messages: Vec::new(),
                text: String::new(),
            })
        } else {
            self.inbox_checked(wait.connection, wait.auth.clone(), peek, || {
                !wait.cancelled()
            })
        };
        if matches!(result, Err(EngineError::StorageUncertain)) {
            self.uncertain = true;
        }
        Reply {
            id,
            result: match result {
                Ok(value) => ReplyResult::Ok(value),
                Err(error) => ReplyResult::Error(error),
            },
        }
    }
    fn wait_state(&mut self, auth: &Auth) -> Arc<WaitState> {
        self.waits
            .entry(mailbox_address(&auth.conversation))
            .or_insert_with(|| {
                Arc::new(WaitState {
                    generation: AtomicU64::new(0),
                    active: AtomicUsize::new(0),
                })
            })
            .clone()
    }
    pub(super) fn stop_waiting(
        &mut self,
        connection: ConnectionId,
        auth: &Auth,
    ) -> Result<Outcome, EngineError> {
        self.authorize(connection, auth)?;
        self.wait_state(auth)
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| EngineError::Limit)?;
        self.changed.notify_waiters();
        Ok(Outcome::WaitingStopped)
    }
}
