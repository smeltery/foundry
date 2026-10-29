//! Cancellation stays available while the owner is consumed by shutdown.
use super::*;

#[derive(Clone)]
pub struct RestorationCancellation {
    requested: Arc<AtomicU8>,
    signals: RunControl,
    sender: mpsc::Sender<CancelMode>,
}
impl RestorationCancellation {
    pub fn cancel(&self, mode: CancelMode) {
        self.requested.fetch_max(
            if mode == CancelMode::Hard { 2 } else { 1 },
            Ordering::AcqRel,
        );
        self.signals.relay(&self.sender);
    }
}
impl RestorationOwner {
    pub fn cancellation_handle(&self) -> RestorationCancellation {
        RestorationCancellation {
            requested: Arc::clone(&self.requested),
            signals: self.signals.clone(),
            sender: self.cancel_tx.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloned_handle_upgrades_once_and_never_downgrades() {
        let requested = Arc::new(AtomicU8::new(0));
        let signals = RunControl::new(Arc::clone(&requested));
        let (sender, received) = mpsc::channel();
        let handle = RestorationCancellation {
            requested,
            signals,
            sender,
        };
        let retained = handle.clone();
        handle.cancel(CancelMode::Graceful);
        drop(handle);
        retained.cancel(CancelMode::Hard);
        retained.cancel(CancelMode::Graceful);
        assert_eq!(received.try_recv().unwrap(), CancelMode::Graceful);
        assert_eq!(received.try_recv().unwrap(), CancelMode::Hard);
        assert!(received.try_recv().is_err());
        assert_eq!(retained.requested.load(Ordering::Acquire), 2);
    }
}
