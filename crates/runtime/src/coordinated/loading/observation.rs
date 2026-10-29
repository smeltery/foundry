use super::*;
use foundry_contracts::context_observation::{ObservationBinding, ObservationFrame};

pub(super) struct ObservationJob {
    binding: ObservationBinding,
    challenge: ContextReceipt,
    unsent: Option<LoadingCommand>,
}
impl LoadedProcess {
    pub fn begin_observation(
        &mut self,
        binding: ObservationBinding,
        cancellation: &Receiver<CancelMode>,
    ) -> Result<(), LoadingError> {
        self.poll(cancellation)?;
        let result = (|| -> Result<ObservationJob, LoadingCause> {
            if self.observation_started || !self.can_observe {
                return Err(
                    RuntimeError::Protocol("observation unavailable for loading owner").into(),
                );
            }
            if binding.scope != self.scope || binding.begin_binding != self.binding {
                return Err(RuntimeError::Protocol("observation expectation mismatch").into());
            }
            let challenge = ContextReceipt::new(uuid::Uuid::new_v4().to_string())
                .map_err(|_| RuntimeError::Invalid("observation challenge unavailable"))?;
            let frame = ObservationFrame::request(binding.clone(), challenge.clone());
            let command = self
                .process
                .as_ref()
                .ok_or(RuntimeError::Invalid("loading owner closed"))?
                .prepare_observation(&frame)?;
            Ok(ObservationJob {
                binding,
                challenge,
                unsent: Some(command),
            })
        })();
        match result {
            Ok(job) => {
                self.observation_started = true;
                self.observation = Some(job);
                Ok(())
            }
            Err(cause) => Err(LoadingError {
                cause,
                cleanup: self.stop().err(),
            }),
        }
    }
    pub fn observed_instance(&self) -> Option<&ContextReceipt> {
        self.instance.as_ref()
    }
    pub fn poll_observation(
        &mut self,
        cancellation: &Receiver<CancelMode>,
    ) -> Result<Option<ObservationFrame>, LoadingError> {
        let result = self.observe_once(cancellation);
        result.map_err(|cause| LoadingError {
            cause,
            cleanup: self.stop().err(),
        })
    }
    fn observe_once(
        &mut self,
        cancellation: &Receiver<CancelMode>,
    ) -> Result<Option<ObservationFrame>, LoadingCause> {
        cancelled(cancellation)?;
        if Instant::now() >= self.deadline {
            return Err(LoadingCause::RetainedTimeout);
        }
        let process = self
            .process
            .as_ref()
            .ok_or(RuntimeError::Invalid("loading owner closed"))?;
        let job = self
            .observation
            .as_mut()
            .ok_or(RuntimeError::Protocol("no pending observation"))?;
        if let Some(command) = job.unsent.take() {
            match process.try_send_loading(command) {
                Ok(()) => {}
                Err(LoadingSendError::Full(command)) => job.unsent = Some(command),
                Err(LoadingSendError::Disconnected) => {
                    return Err(RuntimeError::CommandQueue.into());
                }
            }
        }
        let event = match self.events.try_recv() {
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => return Err(RuntimeError::Exited.into()),
            Ok(event) => event,
        };
        cancelled(cancellation)?;
        if Instant::now() >= self.deadline {
            return Err(LoadingCause::RetainedTimeout);
        }
        let frame = match event {
            ContextOutput::Observation(frame) if job.unsent.is_none() => frame,
            ContextOutput::Ordinary(WireEvent::Error(error)) => return Err(error.into()),
            ContextOutput::Ordinary(WireEvent::Closed) => return Err(RuntimeError::Exited.into()),
            _ => return Err(RuntimeError::Protocol("unexpected observation output").into()),
        };
        frame
            .check_response(&job.binding, &job.challenge)
            .map_err(|_| RuntimeError::Protocol("observation response mismatch"))?;
        if let Some(code) = frame.failure() {
            return Err(LoadingCause::ObservationRejected(code));
        }
        self.instance = frame.instance_id().cloned();
        self.observed_binding = Some(job.binding.clone());
        self.observation = None;
        Ok(Some(frame))
    }
}
