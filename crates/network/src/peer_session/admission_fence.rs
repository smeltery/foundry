use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::watch;

const PENDING: u8 = 0;
const COMMITTING: u8 = 1;
const STOPPED: u8 = 2;
/// Stop and commit contend at one pre-commit linearization point. A database
/// commit already started is drained, never represented as rolled back.
pub struct AdmissionFence {
    phase: AtomicU8,
    cancel: watch::Receiver<bool>,
}
impl AdmissionFence {
    pub fn new(cancel: watch::Receiver<bool>) -> Self {
        Self {
            phase: AtomicU8::new(PENDING),
            cancel,
        }
    }
    pub(super) fn begin_commit(&self) -> bool {
        if *self.cancel.borrow() || self.cancel.has_changed().is_err() {
            return false;
        }
        self.phase
            .compare_exchange(PENDING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn stop_pending(&self) -> bool {
        self.phase
            .compare_exchange(PENDING, STOPPED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub(super) struct StopAdmission(pub std::sync::Arc<AdmissionFence>);
impl Drop for StopAdmission {
    fn drop(&mut self) {
        self.0.stop_pending();
    }
}
