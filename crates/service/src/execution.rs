use crate::{Result, Service, ServiceError, control::RunControl};
use crate::{coordinated::CoordinatedRun, tool_worker::WorkerControl};
use crate::{events::persist_event, run_lease::RunLease};
use foundry_contracts::{AdapterEvent, CancelMode, Lifecycle, MESSAGE_ACK_CAPABILITY, Session};
use foundry_runtime::{
    AdapterSpec, RunResult, coordinated::TrustedToolPolicy, run_adapter_with_messages,
};
use foundry_store::{AttemptFence, Store};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

impl Service {
    /// Run one fenced attempt synchronously. Output is durable before it is accepted.
    pub fn run(&mut self, id: &str, expected_version: u64, spec: &AdapterSpec) -> Result<Session> {
        self.run_with_control(id, expected_version, spec, Arc::new(AtomicU8::new(0)))
    }

    /// Local control: 0 runs, 1 cancels gracefully, 2 cancels hard. Escalation is
    /// monotonic once observed. No signal handler is installed by this service.
    pub fn run_with_control(
        &mut self,
        id: &str,
        expected_version: u64,
        spec: &AdapterSpec,
        control: Arc<AtomicU8>,
    ) -> Result<Session> {
        self.run_inner(id, expected_version, spec, control, None)
    }

    /// Execute explicitly granted workspace tools through the trusted helper.
    /// Existing run entry points remain default-deny.
    pub fn run_with_tools(
        &mut self,
        id: &str,
        expected_version: u64,
        spec: &AdapterSpec,
        control: Arc<AtomicU8>,
        policy: TrustedToolPolicy,
    ) -> Result<Session> {
        self.run_inner(id, expected_version, spec, control, Some(policy))
    }

    fn run_inner(
        &mut self,
        id: &str,
        expected_version: u64,
        spec: &AdapterSpec,
        control: Arc<AtomicU8>,
        policy: Option<TrustedToolPolicy>,
    ) -> Result<Session> {
        let monitor_store = Store::open_with_lease(&self.path, self.options.lease_duration)?;
        // Start before the database operation: lock waits must consume our budget.
        let lease_budget = self.options.lease_duration * 2 / 3;
        let initial_deadline =
            Instant::now()
                .checked_add(lease_budget)
                .ok_or(ServiceError::Invalid(
                    "lease deadline exceeds monotonic clock",
                ))?;
        let fence = self.store.begin_attempt(id, expected_version)?;
        let session = self.store.get(id)?;
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let (message_tx, message_rx) = mpsc::sync_channel(1);
        let signals = RunControl::new(control);
        let accepting = Arc::clone(&signals.accepting);
        if let Some(mode) = signals.relay(&cancel_tx) {
            self.store.request_cancel(id, mode)?;
            return self.finish(&fence, Lifecycle::Suspended, None, None);
        }
        let lease = match RunLease::start(
            monitor_store,
            &fence,
            self.options,
            initial_deadline,
            signals.clone(),
            cancel_tx.clone(),
            message_tx,
        ) {
            Ok(lease) => lease,
            Err(error) => return self.fail_attempt(&fence, error),
        };
        let expired = lease.expired();
        let mut write_error = None;
        let runtime_result = if let Some(policy) = policy {
            let (result, failure) = CoordinatedRun {
                store: &mut self.store,
                fence: &fence,
                session: &session,
                spec,
                cancellation: &cancel_rx,
                messages: &message_rx,
                control: WorkerControl {
                    signals: signals.clone(),
                    activate_on_ready: true,
                    expired: Arc::clone(&expired),
                    cancellation: cancel_tx.clone(),
                },
            }
            .run(policy, || {
                lease.request_stop();
            });
            write_error = failure;
            result
        } else {
            thread::scope(|scope| {
                let (event_tx, event_rx) =
                    mpsc::sync_channel::<(AdapterEvent, mpsc::Sender<bool>)>(1);
                let store = &mut self.store;
                let event_fence = &fence;
                // The supervisor can abandon a blocked write and reap its process before
                // this scoped worker joins. A timed-out write is never accepted as success.
                let writer = scope.spawn(move || {
                    for (event, reply) in event_rx {
                        let result = persist_event(store, event_fence, &event);
                        let _ = reply.send(result.is_ok());
                        result?;
                    }
                    Ok::<(), ServiceError>(())
                });
                let result = run_adapter_with_messages(
                    spec,
                    &session,
                    &fence.attempt_id,
                    &cancel_rx,
                    &message_rx,
                    |event| {
                        if signals.relay(&cancel_tx).is_some() {
                            return;
                        }
                        if write_error.is_some() {
                            return;
                        }
                        if matches!(
                            event,
                            AdapterEvent::Settled { .. } | AdapterEvent::Failed { .. }
                        ) {
                            signals.stop_admission();
                        }
                        let (reply_tx, reply_rx) = mpsc::channel();
                        let result = if event_tx.send((event.clone(), reply_tx)).is_err() {
                            Err(ServiceError::Coordination("event writer unavailable"))
                        } else {
                            loop {
                                if signals.relay(&cancel_tx).is_some() {
                                    return;
                                }
                                if expired.load(Ordering::Acquire) {
                                    break Err(ServiceError::Coordination(
                                        "lease renewal deadline elapsed",
                                    ));
                                }
                                match reply_rx.recv_timeout(Duration::from_millis(5)) {
                                    Ok(true) => break Ok(()),
                                    Ok(false) => {
                                        break Err(ServiceError::Coordination(
                                            "event write failed",
                                        ));
                                    }
                                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                    Err(_) => {
                                        break Err(ServiceError::Coordination(
                                            "event writer unavailable",
                                        ));
                                    }
                                }
                            }
                        };
                        if let Err(error) = result {
                            write_error = Some(error);
                            signals.stop_admission();
                            // Remember the write error even if the process has already settled.
                            let _ = cancel_tx.send(CancelMode::Hard);
                        } else if let AdapterEvent::Ready { capabilities, .. } = event {
                            accepting.store(
                                capabilities
                                    .iter()
                                    .any(|value| value == MESSAGE_ACK_CAPABILITY)
                                    && !expired.load(Ordering::Acquire)
                                    && signals.relay(&cancel_tx).is_none(),
                                Ordering::Release,
                            );
                            // Recheck after publication so a concurrent guard cannot be undone.
                            if expired.load(Ordering::Acquire)
                                || signals.relay(&cancel_tx).is_some()
                            {
                                signals.stop_admission();
                            }
                        }
                    },
                );
                lease.request_stop();
                drop(event_tx);
                match writer.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => write_error = Some(error),
                    Err(_) => {
                        write_error = Some(ServiceError::Coordination("event writer panicked"))
                    }
                }
                result
            })
        };
        // Runtime and persistence workers have cleaned up before lease joins.
        let lease_report = lease.join();
        if lease_report.expired || lease_report.watchdog_panicked {
            write_error = Some(ServiceError::Coordination("lease renewal deadline elapsed"));
        }
        if let Some(mode) = signals.relay(&cancel_tx) {
            self.store.cancellation(&fence)?;
            self.store.request_cancel(id, mode)?;
        }
        let failure = write_error.or_else(|| lease_report.monitor_result.err());
        if let Some(error) = failure {
            return self.fail_attempt(&fence, error);
        }
        let cancellation = self.store.cancellation(&fence)?;
        match runtime_result {
            Ok(RunResult::Cancelled { .. }) => {
                self.finish(&fence, Lifecycle::Suspended, None, None)
            }
            Ok(_) if cancellation.is_some() => {
                self.finish(&fence, Lifecycle::Suspended, None, None)
            }
            Ok(RunResult::Completed { outcome, result }) => {
                self.finish(&fence, Lifecycle::Exit, Some(outcome), Some(result))
            }
            Err(error) => self.fail_attempt(&fence, error.into()),
        }
    }

    fn fail_attempt(&mut self, fence: &AttemptFence, error: ServiceError) -> Result<Session> {
        // If storage or the fence is unavailable, return an error and leave explicit recovery
        // to the caller. Never turn a failed write into a successful run.
        let lifecycle = if self.store.cancellation(fence)?.is_some() {
            Lifecycle::Suspended
        } else {
            Lifecycle::Error
        };
        self.finish(fence, lifecycle, None, None)?;
        Err(error)
    }

    fn finish(
        &mut self,
        fence: &AttemptFence,
        lifecycle: Lifecycle,
        outcome: Option<foundry_contracts::Outcome>,
        result: Option<String>,
    ) -> Result<Session> {
        match self.store.finish_attempt(fence, lifecycle, outcome, result) {
            Ok(session) => Ok(session),
            Err(error) => {
                // Cancellation can commit between the final poll and completion transaction.
                if lifecycle != Lifecycle::Suspended && self.store.cancellation(fence)?.is_some() {
                    Ok(self
                        .store
                        .finish_attempt(fence, Lifecycle::Suspended, None, None)?)
                } else {
                    Err(error.into())
                }
            }
        }
    }
}
