use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const FROZEN: u8 = 1;
const CLAIMING: u8 = 2;

/// One claim owner per run. Freezing is permanent and never waits for storage.
#[derive(Clone, Default)]
pub(crate) struct AdmissionGate(Arc<AtomicU8>);

impl AdmissionGate {
    pub fn try_claim(&self) -> Option<ClaimPermit> {
        self.0
            .compare_exchange(0, CLAIMING, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ClaimPermit(Arc::clone(&self.0)))
    }

    /// Returns true only after every previously admitted claim has released its
    /// permit. Polling this method cannot reopen admission or block on SQLite.
    pub fn freeze(&self) -> bool {
        self.0.fetch_or(FROZEN, Ordering::AcqRel) & CLAIMING == 0
    }
}

/// Retain through the durable claim AND submission to the runtime queue.
pub(crate) struct ClaimPermit(Arc<AtomicU8>);
impl Drop for ClaimPermit {
    fn drop(&mut self) {
        self.0.fetch_and(!CLAIMING, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn empty_freeze_is_immediate_and_cannot_reopen() {
        let gate = AdmissionGate::default();
        assert!(gate.freeze());
        assert!(gate.try_claim().is_none());
        assert!(gate.clone().freeze());
    }

    #[test]
    fn freeze_waits_for_owned_claim_and_queue_submission_without_blocking() {
        let gate = AdmissionGate::default();
        let worker = gate.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let (message_tx, message_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let _permit = worker.try_claim().unwrap();
            entered_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            message_tx.try_send("claimed instruction").unwrap();
        });
        entered_rx.recv().unwrap();
        assert!(!gate.freeze());
        assert!(gate.try_claim().is_none());
        finish_tx.send(()).unwrap();
        thread.join().unwrap();
        assert!(gate.freeze());
        assert_eq!(message_rx.try_recv().unwrap(), "claimed instruction");
        assert!(gate.try_claim().is_none());
    }

    #[test]
    fn failed_claim_releases_ownership_without_clearing_freeze() {
        let gate = AdmissionGate::default();
        let permit = gate.try_claim().unwrap();
        assert!(gate.try_claim().is_none());
        drop(permit);
        let permit = gate.try_claim().unwrap();
        assert!(!gate.freeze());
        drop(permit);
        assert!(gate.freeze());
        assert!(gate.try_claim().is_none());
    }
}
