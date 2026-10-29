use crate::{Result, ServiceError, control::RunControl, events::persist_event, tool_digest};
use foundry_contracts::{AdapterEvent, CancelMode, ContractError, MESSAGE_ACK_CAPABILITY};
use foundry_runtime::coordinated::{
    CheckpointId, MAX_TOOL_CALLS, PersistenceFailure, PersistenceReply, PersistenceRequest,
    ToolDisposition, ToolEvidence, ToolIntent,
};
use foundry_store::{AttemptFence, Store, StoreError, ToolOutcome, ToolRecord};
use foundry_tools::host::{self, Effect, Input, Outcome};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[cfg(test)]
#[path = "tool_receipt_tests.rs"]
mod tests;

pub(crate) struct WorkerControl {
    pub signals: RunControl,
    pub permissions: Option<Arc<crate::permission::ToolPermissions>>,
    /// Capture owners activate only after their correlated startup barrier.
    pub activate_on_ready: bool,
    pub expired: Arc<AtomicBool>,
    pub cancellation: mpsc::Sender<CancelMode>,
}
impl WorkerControl {
    fn revoked(&self) -> bool {
        self.signals.relay(&self.cancellation).is_some() || self.expired.load(Ordering::Acquire)
    }
}

/// A single bounded worker owns every claim/receipt write. It never spawns helpers.
pub(crate) fn run(
    store: &mut Store,
    fence: &AttemptFence,
    requests: mpsc::Receiver<PersistenceRequest>,
    replies: mpsc::SyncSender<PersistenceReply>,
    control: WorkerControl,
) -> Result<()> {
    let mut state = PersistenceState::default();
    for request in requests {
        state.handle(store, fence, request, &replies, &control)?;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct PersistenceState {
    active: Option<(CheckpointId, ToolRecord)>,
    count: usize,
}
impl PersistenceState {
    fn claim_authorized(
        &mut self,
        store: &mut Store,
        fence: &AttemptFence,
        checkpoint: CheckpointId,
        intent: &ToolIntent,
        control: &WorkerControl,
    ) -> Result<PersistenceReply> {
        validate_intent(intent, self.active.is_some(), self.count)?;
        let authority = match &control.permissions {
            Some(permissions) => match permissions.prepare(fence, intent, &control.signals)? {
                crate::permission::PreparedPermission::Blocked(disposition) => {
                    return Ok(PersistenceReply::ToolBlocked {
                        checkpoint,
                        disposition,
                    });
                }
                crate::permission::PreparedPermission::Authorized(authority) => Some(authority),
            },
            None => None,
        };
        let record = claim(store, fence, intent, self.active.is_some(), self.count)?;
        self.active = Some((checkpoint, record));
        self.count += 1;
        Ok(match authority {
            Some(authorization) => PersistenceReply::ToolAuthorized {
                checkpoint,
                authorization,
            },
            None => PersistenceReply::ToolClaimed { checkpoint },
        })
    }

    pub(crate) fn idle(&self) -> bool {
        self.active.is_none()
    }
    pub(crate) fn handle(
        &mut self,
        store: &mut Store,
        fence: &AttemptFence,
        request: PersistenceRequest,
        replies: &mpsc::SyncSender<PersistenceReply>,
        control: &WorkerControl,
    ) -> Result<()> {
        let checkpoint = match &request {
            PersistenceRequest::Event { checkpoint, .. }
            | PersistenceRequest::ClaimTool { checkpoint, .. }
            | PersistenceRequest::FinishTool { checkpoint, .. }
            | PersistenceRequest::FinishToolWithEvidence { checkpoint, .. } => *checkpoint,
        };
        if control.revoked() {
            send(
                replies,
                PersistenceReply::Rejected {
                    checkpoint,
                    code: PersistenceFailure::OwnershipLost,
                },
            )?;
            return Ok(());
        }
        if matches!(
            &request,
            PersistenceRequest::Event {
                event: AdapterEvent::Settled { .. } | AdapterEvent::Failed { .. },
                ..
            }
        ) {
            control.signals.stop_admission();
        }
        let ready = match &request {
            PersistenceRequest::Event {
                event: AdapterEvent::Ready { capabilities, .. },
                ..
            } => capabilities
                .iter()
                .any(|value| value == MESSAGE_ACK_CAPABILITY),
            _ => false,
        };
        let include_evidence =
            matches!(&request, PersistenceRequest::FinishToolWithEvidence { .. });
        let result = match request {
            PersistenceRequest::Event { event, .. } => validate_event(fence, &event)
                .and_then(|()| persist_event(store, fence, &event))
                .map(|()| PersistenceReply::EventCommitted { checkpoint }),
            PersistenceRequest::ClaimTool { intent, .. } => {
                self.claim_authorized(store, fence, checkpoint, &intent, control)
            }
            PersistenceRequest::FinishTool { evidence, .. }
            | PersistenceRequest::FinishToolWithEvidence { evidence, .. } => {
                finish(store, fence, checkpoint, &self.active, evidence).map(
                    |(disposition, digest)| {
                        self.active = None;
                        if include_evidence {
                            PersistenceReply::ToolFinishedEvidence {
                                checkpoint,
                                disposition,
                                output_digest: digest,
                            }
                        } else {
                            PersistenceReply::ToolFinished {
                                checkpoint,
                                disposition,
                            }
                        }
                    },
                )
            }
        };
        let reply = match result {
            Ok(reply) if !control.revoked() => reply,
            Ok(_) => PersistenceReply::Rejected {
                checkpoint,
                code: PersistenceFailure::OwnershipLost,
            },
            Err(error) => {
                let _ = send(
                    replies,
                    PersistenceReply::Rejected {
                        checkpoint,
                        code: failure(&error),
                    },
                );
                control.signals.stop_admission();
                let _ = control.cancellation.send(CancelMode::Hard);
                return Err(error);
            }
        };
        if ready && control.activate_on_ready && !control.revoked() {
            control.signals.accepting.store(true, Ordering::Release);
            if control.revoked() {
                control.signals.stop_admission();
            }
        }
        if let Err(error) = send(replies, reply) {
            control.signals.stop_admission();
            let _ = control.cancellation.send(CancelMode::Hard);
            return Err(error);
        }
        Ok(())
    }
}

fn send(replies: &mpsc::SyncSender<PersistenceReply>, reply: PersistenceReply) -> Result<()> {
    match replies.try_send(reply) {
        Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => Err(ServiceError::Coordination(
            "persistence reply queue is full",
        )),
    }
}

fn claim(
    store: &mut Store,
    fence: &AttemptFence,
    intent: &ToolIntent,
    active: bool,
    count: usize,
) -> Result<ToolRecord> {
    validate_intent(intent, active, count)?;
    let digest = tool_digest::input(&intent.tool, &intent.input)?;
    Ok(store.claim_tool(fence, &intent.call_id, &intent.tool, digest)?)
}

fn validate_intent(intent: &ToolIntent, active: bool, count: usize) -> Result<()> {
    if active || count >= MAX_TOOL_CALLS {
        return Err(invalid("tool admission limit exceeded"));
    }
    if !matches!(
        (&*intent.tool, &intent.input),
        ("read" | "list", Input::Path(_)) | ("write", Input::Write(_))
    ) {
        return Err(invalid("tool input does not match tool"));
    }
    let bytes = serde_json::to_vec(intent).map_err(StoreError::Json)?;
    if bytes.len() > host::MAX_REQUEST_BYTES {
        return Err(invalid("tool request exceeds byte limit"));
    }
    Ok(())
}

fn finish(
    store: &mut Store,
    fence: &AttemptFence,
    checkpoint: CheckpointId,
    active: &Option<(CheckpointId, ToolRecord)>,
    evidence: ToolEvidence,
) -> Result<(ToolDisposition, Option<[u8; 32]>)> {
    let Some((expected, record)) = active else {
        return Err(invalid("tool completion has no claim"));
    };
    if *expected != checkpoint {
        return Err(invalid("tool completion checkpoint mismatch"));
    }
    let (outcome, digest) = match evidence {
        ToolEvidence::FailedBeforeSpawn => (ToolOutcome::Failed, None),
        ToolEvidence::Uncertain => (ToolOutcome::Uncertain, None),
        ToolEvidence::Replied(reply) => validate_reply(fence, record, &reply)?,
        ToolEvidence::SharedReply(reply) => validate_reply(fence, record, reply.reply())?,
    };
    store.finish_tool(fence, &record.call_id, outcome, digest)?;
    let disposition = match outcome {
        ToolOutcome::Success => ToolDisposition::Success,
        ToolOutcome::Failed => ToolDisposition::NoEffectFailure,
        ToolOutcome::Uncertain => ToolDisposition::Uncertain,
    };
    Ok((disposition, digest))
}

fn validate_reply(
    fence: &AttemptFence,
    record: &ToolRecord,
    reply: &host::Reply,
) -> Result<(ToolOutcome, Option<[u8; 32]>)> {
    if reply.version != host::VERSION
        || reply.attempt_id != fence.attempt_id
        || reply.call_id != record.call_id
    {
        return Err(invalid("tool completion identity mismatch"));
    }
    serde_json::to_writer(ReplyBytes(0), reply)
        .map_err(|_| invalid("tool reply exceeds byte limit"))?;
    let outcome = match &reply.result {
        Outcome::Ok { .. } => ToolOutcome::Success,
        Outcome::Error {
            effect: Effect::None,
            ..
        } => ToolOutcome::Failed,
        Outcome::Error {
            effect: Effect::Unknown,
            ..
        } => ToolOutcome::Uncertain,
    };
    Ok((outcome, Some(tool_digest::output(reply)?)))
}

// Check encoded length before the ledger digest allocates its canonical value.
struct ReplyBytes(usize);
impl std::io::Write for ReplyBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > host::MAX_REPLY_BYTES.saturating_sub(self.0) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_event(fence: &AttemptFence, event: &AdapterEvent) -> Result<()> {
    let attempt = match event {
        AdapterEvent::Ready { .. } => return Ok(()),
        AdapterEvent::Text { attempt_id, .. }
        | AdapterEvent::ToolCall { attempt_id, .. }
        | AdapterEvent::Settled { attempt_id, .. }
        | AdapterEvent::Failed { attempt_id, .. }
        | AdapterEvent::MessageAccepted { attempt_id, .. } => attempt_id,
    };
    if attempt != &fence.attempt_id {
        return Err(invalid("event attempt mismatch"));
    }
    Ok(())
}
fn invalid(message: &'static str) -> ServiceError {
    ServiceError::Store(ContractError::Invalid(message.into()).into())
}
fn failure(error: &ServiceError) -> PersistenceFailure {
    match error {
        ServiceError::Store(StoreError::Contract(ContractError::Invalid(_))) => {
            PersistenceFailure::Invalid
        }
        ServiceError::Store(StoreError::Contract(ContractError::Conflict(_))) => {
            PersistenceFailure::Conflict
        }
        _ => PersistenceFailure::Storage,
    }
}
