use foundry_contracts::CancelMode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc::Sender,
};

#[derive(Clone)]
pub(crate) struct RunControl {
    pub accepting: Arc<AtomicBool>,
    pub admission: crate::admission::AdmissionGate,
    pub permissions: crate::permission_gate::PermissionGate,
    requested: Arc<AtomicU8>,
    observed: Arc<AtomicU8>,
}
impl RunControl {
    pub fn new(requested: Arc<AtomicU8>) -> Self {
        Self {
            permissions: crate::permission_gate::PermissionGate::new(Arc::clone(&requested)),
            requested,
            observed: Arc::new(AtomicU8::new(0)),
            accepting: Arc::new(AtomicBool::new(false)),
            admission: crate::admission::AdmissionGate::default(),
        }
    }

    /// Permanently stop new claims without waiting for a claim already in flight.
    pub fn stop_admission(&self) {
        self.permissions.revoke();
        self.admission.freeze();
        self.accepting.store(false, Ordering::Release);
    }

    /// Observation is sticky even if a caller accidentally decreases its atomic.
    /// Notify the process supervisor before any potentially blocking storage work.
    pub fn relay(&self, sender: &Sender<CancelMode>) -> Option<CancelMode> {
        let requested = self.requested.load(Ordering::Acquire).min(2);
        let previous = self.observed.fetch_max(requested, Ordering::AcqRel);
        let level = previous.max(requested);
        let mode = match level {
            0 => None,
            1 => Some(CancelMode::Graceful),
            _ => Some(CancelMode::Hard),
        };
        if level > 0 {
            self.stop_admission();
        }
        if requested > previous
            && let Some(mode) = mode
        {
            // A closed receiver means supervision has returned. A later owner admission
            // still observes this state; an already admitted terminal result may win.
            let _ = sender.send(mode);
        }
        mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_control_never_downgrades_or_resends_same_mode() {
        let requested = Arc::new(AtomicU8::new(1));
        let signals = RunControl::new(Arc::clone(&requested));
        let (sender, receiver) = std::sync::mpsc::channel();
        assert_eq!(signals.relay(&sender), Some(CancelMode::Graceful));
        assert_eq!(receiver.try_recv().unwrap(), CancelMode::Graceful);
        requested.store(0, Ordering::Release);
        assert_eq!(signals.relay(&sender), Some(CancelMode::Graceful));
        assert!(receiver.try_recv().is_err());
        requested.store(2, Ordering::Release);
        assert_eq!(signals.relay(&sender), Some(CancelMode::Hard));
        assert_eq!(receiver.try_recv().unwrap(), CancelMode::Hard);
        requested.store(1, Ordering::Release);
        assert_eq!(signals.relay(&sender), Some(CancelMode::Hard));
        assert!(receiver.try_recv().is_err());
    }
}
