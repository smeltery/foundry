use super::*;
use crate::coordinated::capture::DeliveredTrace;
use std::{collections::VecDeque, sync::mpsc::TrySendError};

enum Phase {
    Grant,
    Accepted,
    Activation,
    Admitted,
}
#[must_use = "poll or explicitly close the continuation owner"]
pub struct PendingContinuation {
    pub(super) loaded: Option<LoadedProcess>,
    binding: Arc<ContinuationBinding>,
    owner: ContinuationOwner,
    pub(super) job: ContinuationJob,
    pub(super) receipt: ContextReceipt,
    pending: Option<ContinuationRequest>,
    command: Option<LoadingCommand>,
    identity: Option<ContinuationIdentity>,
    delta: Option<DeliveredTrace>,
    phase: Phase,
    buffered: VecDeque<ContextOutput>,
}
impl PendingContinuation {
    pub(super) fn new(
        loaded: LoadedProcess,
        binding: ContinuationBinding,
        owner: ContinuationOwner,
    ) -> Result<Self, LoadingError> {
        let job = ContinuationJob(uuid::Uuid::new_v4());
        let receipt =
            ContextReceipt::new(uuid::Uuid::new_v4().to_string()).map_err(|_| LoadingError {
                cause: RuntimeError::Invalid("continuation receipt unavailable").into(),
                cleanup: None,
            })?;
        let binding = Arc::new(binding);
        let pending = Some(ContinuationRequest::Authorize {
            job: job.clone(),
            binding: Arc::clone(&binding),
            tail_receipt_digest: ContextDigest::of(receipt.as_str().as_bytes()),
        });
        Ok(Self {
            loaded: Some(loaded),
            binding,
            owner,
            job,
            receipt,
            pending,
            command: None,
            identity: None,
            delta: None,
            phase: Phase::Grant,
            buffered: VecDeque::new(),
        })
    }
    pub fn cleanup_observer(&self) -> CleanupObserver {
        self.loaded.as_ref().unwrap().cleanup_observer()
    }
    pub fn process_id(&self) -> u32 {
        self.loaded.as_ref().map_or(0, LoadedProcess::process_id)
    }
    pub fn close(mut self) -> Result<(), RuntimeError> {
        self.loaded.as_mut().map_or(Ok(()), LoadedProcess::stop)
    }
    pub fn poll(&mut self, cancellation: &Receiver<CancelMode>) -> Result<bool, LoadingError> {
        let result = self.poll_once(cancellation);
        result.map_err(|cause| LoadingError {
            cause,
            cleanup: self.loaded.as_mut().and_then(|l| l.stop().err()),
        })
    }
    fn poll_once(&mut self, cancellation: &Receiver<CancelMode>) -> Result<bool, LoadingCause> {
        cancelled(cancellation)?;
        let loaded = self
            .loaded
            .as_mut()
            .ok_or(RuntimeError::Invalid("continuation owner transferred"))?;
        if Instant::now() >= loaded.deadline {
            return Err(LoadingCause::RetainedTimeout);
        }
        let process = loaded
            .process
            .as_ref()
            .ok_or(RuntimeError::Invalid("continuation owner closed"))?;
        if let Some(request) = self.pending.take() {
            match self.owner.requests.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => self.pending = Some(request),
                Err(TrySendError::Disconnected(_)) => {
                    return Err(RuntimeError::Protocol("continuation owner unavailable").into());
                }
            }
        }
        if let Some(command) = self.command.take() {
            match process.try_send_loading(command) {
                Ok(()) => {}
                Err(LoadingSendError::Full(command)) => self.command = Some(command),
                Err(LoadingSendError::Disconnected) => {
                    return Err(RuntimeError::CommandQueue.into());
                }
            }
        }
        // Consume wire before owner activation, so queued bad output cannot be hidden.
        for _ in 0..=64 {
            match loaded.events.try_recv() {
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(RuntimeError::Exited.into()),
                Ok(ContextOutput::Loading(frame))
                    if matches!(self.phase, Phase::Accepted) && self.command.is_none() =>
                {
                    let identity = self.identity.as_ref().unwrap();
                    frame
                        .check_accepted(&loaded.scope, identity, &self.receipt)
                        .map_err(|_| RuntimeError::Protocol("continuation acceptance mismatch"))?;
                    self.pending = Some(ContinuationRequest::Accept {
                        job: self.job.clone(),
                        scope: loaded.scope.clone(),
                        identity: identity.clone(),
                        receipt: self.receipt.clone(),
                    });
                    self.phase = Phase::Activation;
                }
                Ok(output @ ContextOutput::Ordinary(WireEvent::Event(_)))
                    if matches!(self.phase, Phase::Activation | Phase::Admitted) =>
                {
                    if self.buffered.len() >= 64 {
                        return Err(
                            RuntimeError::Protocol("continuation output buffer exceeded").into(),
                        );
                    }
                    self.buffered.push_back(output);
                }
                Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => return Err(error.into()),
                Ok(ContextOutput::Ordinary(WireEvent::Closed)) => {
                    return Err(RuntimeError::Exited.into());
                }
                Ok(_) => {
                    return Err(
                        RuntimeError::Protocol("unexpected pending continuation output").into(),
                    );
                }
            }
        }
        match self.owner.replies.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                return Err(RuntimeError::Protocol("continuation owner disconnected").into());
            }
            Ok(reply) => {
                if self.pending.is_some() {
                    return Err(
                        RuntimeError::Protocol("unsolicited continuation owner reply").into(),
                    );
                }
                cancelled(cancellation)?;
                match reply {
                    ContinuationReply::Granted {
                        job,
                        identity,
                        text,
                    } if job == self.job && matches!(self.phase, Phase::Grant) => {
                        let frame = LoadingFrame::continuation(
                            loaded.scope.clone(),
                            identity.clone(),
                            &text,
                            self.receipt.clone(),
                        )
                        .map_err(|_| RuntimeError::Invalid("invalid granted continuation"))?;
                        let delta = DeliveredTrace::restored_instruction(&identity, &text)
                            .map_err(|_| RuntimeError::Invalid("invalid continuation delta"))?;
                        self.command = Some(process.prepare_continuation(&frame)?);
                        self.delta = Some(delta);
                        self.identity = Some(identity);
                        self.phase = Phase::Accepted;
                    }
                    ContinuationReply::Activated { job }
                        if job == self.job && matches!(self.phase, Phase::Activation) =>
                    {
                        self.delta.as_mut().unwrap().confirm_continuation();
                        self.phase = Phase::Admitted;
                    }
                    _ => {
                        return Err(
                            RuntimeError::Protocol("continuation owner reply rejected").into()
                        );
                    }
                }
                if !matches!(self.owner.replies.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(RuntimeError::Protocol(
                        "duplicate or disconnected continuation owner reply",
                    )
                    .into());
                }
            }
        }
        cancelled(cancellation)?;
        if Instant::now() >= loaded.deadline {
            return Err(LoadingCause::RetainedTimeout);
        }
        Ok(matches!(self.phase, Phase::Admitted))
    }
    pub fn into_admitted(mut self) -> Result<AdmittedProcess, LoadingError> {
        if !matches!(self.phase, Phase::Admitted) {
            return Err(LoadingError {
                cause: RuntimeError::Invalid("continuation not admitted").into(),
                cleanup: self.loaded.as_mut().and_then(|l| l.stop().err()),
            });
        }
        Ok(AdmittedProcess {
            loaded: self.loaded.take().unwrap(),
            binding: Arc::clone(&self.binding),
            identity: self.identity.take().unwrap(),
            delta: self.delta.take().unwrap(),
            buffered: self.buffered,
        })
    }
}
