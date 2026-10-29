use super::*;

impl Coordinator<'_> {
    pub(super) fn supervise(
        &mut self,
        cancellation: &Receiver<CancelMode>,
        messages: &Receiver<AdapterMessage>,
    ) -> Result<Proposal, RuntimeError> {
        let started = Instant::now();
        let mut stopping = None;
        let mut observed_control = None;
        loop {
            if self.restored.is_some() && observed_control.is_none() {
                match cancellation.try_recv() {
                    Ok(mode) => observed_control = Some(mode),
                    Err(TryRecvError::Disconnected) => {
                        return Err(protocol("restored owner disconnected"));
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            while let Some(mode) = observed_control
                .take()
                .or_else(|| cancellation.try_recv().ok())
            {
                self.stop.store(true, Ordering::Release);
                if let Some(helper) = self.job.as_mut().and_then(|job| job.helper.as_mut()) {
                    helper.request_stop();
                }
                if mode == CancelMode::Hard {
                    return Ok(RunResult::Cancelled { mode, forced: true }.into());
                }
                if stopping.is_none() {
                    stopping = Some(Instant::now());
                    if !self.ready {
                        return Ok(RunResult::Cancelled {
                            mode,
                            forced: false,
                        }
                        .into());
                    }
                    self.process.send(&AdapterCommand::Cancel {
                        attempt_id: self.attempt.into(),
                        mode,
                    })?;
                }
            }
            if let Some(since) = stopping {
                if since.elapsed() >= self.spec.cancel_timeout {
                    return Ok(RunResult::Cancelled {
                        mode: CancelMode::Graceful,
                        forced: true,
                    }
                    .into());
                }
                match self.ingress.try_recv(&self.process.events) {
                    Ok(ContextOutput::Ordinary(WireEvent::Closed))
                    | Err(TryRecvError::Disconnected) => {
                        return Ok(RunResult::Cancelled {
                            mode: CancelMode::Graceful,
                            forced: false,
                        }
                        .into());
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Event(
                        AdapterEvent::Settled { .. } | AdapterEvent::Failed { .. },
                    ))) => {
                        return Ok(RunResult::Cancelled {
                            mode: CancelMode::Graceful,
                            forced: false,
                        }
                        .into());
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => return Err(error),
                    _ => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                }
            }
            if !self.ready && started.elapsed() >= self.spec.ready_timeout {
                return Err(RuntimeError::ReadyTimeout);
            }
            if started.elapsed() >= self.spec.run_timeout {
                return Err(RuntimeError::RunTimeout);
            }
            if let Some(startup) = self.startup.as_mut() {
                let admitted = startup.poll(&self.process, self.session)?;
                if let Ok(mode) = cancellation.try_recv() {
                    observed_control = Some(mode);
                    continue;
                }
                if let Some(enabled) = admitted {
                    self.delivery.enabled = enabled;
                }
            }
            if let Some(job) = self.job.as_mut() {
                job.poll()?;
            }
            if let Some(event) = self.event.as_mut() {
                submit(&self.persistence, &mut event.pending)?;
            }
            if let Some(job) = self.job.as_mut() {
                submit(&self.persistence, &mut job.pending)?;
            }
            match self.persistence.replies.try_recv() {
                Ok(reply) => {
                    // Recheck before consuming authority, not just at the loop boundary.
                    if let Ok(mode) = cancellation.try_recv() {
                        observed_control = Some(mode);
                        continue;
                    }
                    if let Some(result) = self.persisted(reply)? {
                        return Ok(result.into());
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(protocol("persistence reply channel closed"));
                }
                Err(TryRecvError::Empty) => {}
            }
            let terminal = self.startup.as_ref().is_some_and(|s| s.terminal_pending());
            if terminal {
                if messages.try_recv().is_ok() {
                    return Err(protocol("queued instruction at capture cut"));
                }
                if self.job.is_some() {
                    return Err(protocol("outstanding capture tool work"));
                }
                if self.event.is_none() {
                    self.startup.as_mut().unwrap().advance_cut()?;
                }
            }
            if (self.event.is_none() || terminal)
                && !self
                    .startup
                    .as_ref()
                    .is_some_and(|startup| startup.waiting_activation())
            {
                self.delivery.forward_traced(
                    messages,
                    &self.process,
                    self.attempt,
                    self.startup
                        .as_mut()
                        .map(|s| &mut s.trace)
                        .or(self.restored.as_mut()),
                )?;
                match self.ingress.try_recv(&self.process.events) {
                    Ok(ContextOutput::Ordinary(WireEvent::Event(event))) => self.event(event)?,
                    Ok(ContextOutput::Loading(_) | ContextOutput::Observation(_)) => {
                        return Err(protocol("loading frame outside loading controller"));
                    }
                    Ok(ContextOutput::Context(event)) => {
                        self.startup
                            .as_mut()
                            .ok_or_else(|| protocol("unexpected context phase"))?
                            .context(event)?;
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => return Err(error),
                    Ok(ContextOutput::Ordinary(WireEvent::Closed))
                    | Err(TryRecvError::Disconnected) => {
                        return Err(RuntimeError::Exited);
                    }
                    Err(TryRecvError::Empty) => {
                        if let Ok(mode) = cancellation.try_recv() {
                            observed_control = Some(mode);
                            continue;
                        }
                        // Check already queued adapter events before releasing a durable
                        // result. Settlement observed with an outstanding job is invalid.
                        if let Some(job) = self.job.as_mut()
                            && job.deliver(
                                &self.process,
                                self.attempt,
                                self.startup
                                    .as_mut()
                                    .map(|s| &mut s.trace)
                                    .or(self.restored.as_mut()),
                            )?
                        {
                            self.job.take();
                        }
                    }
                }
            }
            if terminal
                && self.event.is_none()
                && let Some(verified) = self.startup.as_mut().unwrap().take_verified()
            {
                if let Ok(mode) = cancellation.try_recv() {
                    return Ok(RunResult::Cancelled {
                        mode,
                        forced: false,
                    }
                    .into());
                }
                self.startup.as_ref().unwrap().check_owner_drained()?;
                return Ok(Proposal::Validated(Box::new(verified)));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
