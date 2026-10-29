use crate::{Result, ServiceError, control::RunControl};
use foundry_contracts::CancelMode;
use foundry_runtime::AdapterMessage;
use foundry_store::{AttemptFence, Store};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub(crate) struct LeaseRenewal {
    pub settlement: Arc<Mutex<()>>,
    pub interval: Duration,
    pub budget: Duration,
    pub updates: mpsc::Sender<Option<Instant>>,
    pub expired: Arc<AtomicBool>,
}

pub(crate) fn monitor_attempt(
    mut store: Store,
    fence: &AttemptFence,
    lease: LeaseRenewal,
    stop: mpsc::Receiver<()>,
    cancellation: &mpsc::Sender<CancelMode>,
    messages: mpsc::SyncSender<AdapterMessage>,
    signals: RunControl,
) -> Result<()> {
    let mut delivered = None;
    loop {
        let guard = lease
            .settlement
            .lock()
            .map_err(|_| ServiceError::Coordination("lease settlement lock poisoned"))?;
        match stop.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let local = signals.relay(cancellation);
        let deadline =
            Instant::now()
                .checked_add(lease.budget)
                .ok_or(ServiceError::Coordination(
                    "lease deadline exceeds monotonic clock",
                ))?;
        store.heartbeat(fence)?;
        if lease.expired.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination("lease renewal deadline elapsed"));
        }
        if lease.updates.send(Some(deadline)).is_err() {
            return Ok(());
        }
        if let Some(mode) = local {
            store.request_cancel(&fence.session_id, mode)?;
        }
        let cancel = store.cancellation(fence)?;
        if cancel.is_some() {
            signals.stop_admission();
        }
        if let Some(mode) = cancel
            && delivered != Some(mode)
        {
            // Runtime cleanup can precede owner finalization. Keep renewing until
            // the lease owner explicitly stops, even when its runtime has returned.
            let _ = cancellation.send(mode);
            delivered = Some(mode);
        }
        // Retain the permit through the durable claim and queue submission;
        // a freeze acknowledgement must not race either operation.
        if cancel.is_none()
            && signals.accepting.load(Ordering::Acquire)
            && let Some(_permit) = signals.admission.try_claim()
            && let Some(message) = store.claim_next_message(fence)?
        {
            messages
                .try_send(AdapterMessage {
                    message_id: message.id,
                    text: message.text,
                })
                .map_err(|_| {
                    ServiceError::Coordination("claimed message could not be submitted")
                })?;
        }
        drop(guard);
        match stop.recv_timeout(lease.interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

pub(crate) fn watch_lease(
    mut deadline: Instant,
    updates: mpsc::Receiver<Option<Instant>>,
    expired: Arc<AtomicBool>,
    cancellation: mpsc::Sender<CancelMode>,
    signals: RunControl,
) {
    signals.permissions.initialize(deadline);
    loop {
        signals.relay(&cancellation);
        // Once the old budget expires, even a late successful renewal cannot revive it.
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match updates.recv_timeout((deadline - now).min(Duration::from_millis(5))) {
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Some(next)) if signals.permissions.renew(deadline, next) => {
                deadline = next;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            _ => break,
        }
    }
    signals.stop_admission();
    expired.store(true, Ordering::Release);
    let _ = cancellation.send(CancelMode::Hard);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU8;

    #[test]
    fn watchdog_closes_stale_admission_without_waiting_for_prior_claims() {
        for prior_claim in [false, true] {
            let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
            signals.accepting.store(true, Ordering::Release);
            // Model a monitor paused after its advisory flag read.
            let stale_accepting = signals.accepting.load(Ordering::Acquire);
            let permit = prior_claim.then(|| signals.admission.try_claim().unwrap());
            let expired = Arc::new(AtomicBool::new(false));
            let (_updates, receiver) = mpsc::channel();
            let (cancel, cancelled) = mpsc::channel();
            watch_lease(
                Instant::now(),
                receiver,
                expired.clone(),
                cancel,
                signals.clone(),
            );
            assert!(expired.load(Ordering::Acquire));
            assert_eq!(cancelled.try_recv().unwrap(), CancelMode::Hard);
            assert!(stale_accepting);
            assert!(!signals.accepting.load(Ordering::Acquire));
            assert!(signals.admission.try_claim().is_none());
            // Even a late Ready publication cannot reopen the permanent gate.
            signals.accepting.store(true, Ordering::Release);
            assert!(signals.admission.try_claim().is_none());
            assert_eq!(signals.admission.freeze(), !prior_claim);
            drop(permit);
            assert!(signals.admission.freeze());
        }
    }
}
