use crate::lease::{LeaseRenewal, monitor_attempt, watch_lease};
use crate::{Result, ServiceError, ServiceOptions, control::RunControl};
use foundry_contracts::CancelMode;
use foundry_runtime::AdapterMessage;
use foundry_store::{AttemptFence, Store};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

/// Owns renewal threads; freezing claims does not end lease coverage.
pub(crate) struct RunLease {
    signals: RunControl,
    expired: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    renew: mpsc::Sender<Option<Instant>>,
    watchdog: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<Result<()>>>,
}

pub(crate) struct LeaseReport {
    pub expired: bool,
    pub watchdog_panicked: bool,
    pub monitor_result: Result<()>,
}

impl RunLease {
    /// initial_deadline includes time spent waiting for admission's database lock.
    pub fn start(
        store: Store,
        fence: &AttemptFence,
        options: ServiceOptions,
        initial_deadline: Instant,
        signals: RunControl,
        cancellation: mpsc::Sender<CancelMode>,
        messages: mpsc::SyncSender<AdapterMessage>,
    ) -> Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (renew, renewed) = mpsc::channel();
        let expired = Arc::new(AtomicBool::new(false));
        let watchdog_expired = Arc::clone(&expired);
        let watchdog_cancel = cancellation.clone();
        let watchdog_signals = signals.clone();
        let watchdog = thread::Builder::new()
            .name("foundry-lease-deadline".into())
            .spawn(move || {
                watch_lease(
                    initial_deadline,
                    renewed,
                    watchdog_expired,
                    watchdog_cancel,
                    watchdog_signals,
                )
            })
            .map_err(|_| ServiceError::Coordination("cannot start lease watchdog"))?;
        let mut lease = Self {
            signals: signals.clone(),
            expired: Arc::clone(&expired),
            stop,
            renew,
            watchdog: Some(watchdog),
            monitor: None,
        };
        let updates = lease.renew.clone();
        let fence = fence.clone();
        let monitor = thread::Builder::new()
            .name("foundry-lease".into())
            .spawn(move || {
                let result = monitor_attempt(
                    store,
                    &fence,
                    LeaseRenewal {
                        interval: options.poll_interval,
                        budget: options.lease_duration * 2 / 3,
                        updates,
                        expired,
                    },
                    stopped,
                    &cancellation,
                    messages,
                    signals.clone(),
                );
                signals.stop_admission();
                if result.is_err() {
                    let _ = cancellation.send(CancelMode::Hard);
                }
                result
            });
        match monitor {
            Ok(monitor) => {
                lease.monitor = Some(monitor);
                Ok(lease)
            }
            Err(_) => {
                // Dropping the partially initialized owner stops and joins its watchdog.
                Err(ServiceError::Coordination("cannot start lease monitor"))
            }
        }
    }

    pub fn expired(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.expired)
    }

    pub fn freeze_claims(&self) {
        self.signals.stop_admission();
    }

    pub fn request_stop(&self) {
        self.freeze_claims();
        let _ = self.renew.send(None);
        let _ = self.stop.send(());
    }

    /// Explicit reporting path. Both joins are attempted even after a panic.
    pub fn join(mut self) -> LeaseReport {
        self.request_stop();
        self.join_threads()
    }

    fn join_threads(&mut self) -> LeaseReport {
        let watchdog_panicked = self
            .watchdog
            .take()
            .is_some_and(|worker| worker.join().is_err());
        let monitor_result = match self.monitor.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(ServiceError::Coordination("lease monitor panicked")),
            None => Ok(()),
        };
        LeaseReport {
            expired: self.expired.load(Ordering::Acquire),
            watchdog_panicked,
            monitor_result,
        }
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        self.request_stop();
        // Unwind/early-return fallback only; normal callers must inspect join().
        // SQLite waits can delay this cleanup; it is not a hard-timeout guarantee.
        let _ = self.join_threads();
    }
}

#[cfg(test)]
#[path = "run_lease_tests.rs"]
mod tests;
