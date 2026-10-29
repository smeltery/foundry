//! Shared reader phase. No lock is held across a pipe or channel operation.
use super::*;
use foundry_contracts::context::ContextReceipt;
use foundry_contracts::context_v2::{ContinuationIdentity, LoadingFrame, LoadingScope};

pub(crate) const BUFFER_EVENTS: usize = 64;
pub(crate) const BUFFER_BYTES: usize = 262_144;
pub(super) enum Latch {
    Loading,
    AwaitAccepted {
        identity: ContinuationIdentity,
        receipt: ContextReceipt,
    },
    Buffered {
        attempt: String,
        events: usize,
        bytes: usize,
    },
    Admitted {
        attempt: String,
        capture: bool,
    },
    Failed,
}
pub(super) fn decode(
    bytes: &[u8],
    scope: &LoadingScope,
    ready: &mut bool,
    latch: &Mutex<Latch>,
    receipts: &Mutex<HashMap<(String, String), String>>,
) -> Result<ContextOutput, RuntimeError> {
    let mut phase = latch
        .lock()
        .map_err(|_| RuntimeError::Protocol("continuation phase unavailable"))?;
    let result = match &mut *phase {
        Latch::Loading => super::loading::decode_loading(bytes, scope, ready),
        Latch::AwaitAccepted { identity, receipt } => {
            let identity = identity.clone();
            let receipt = receipt.clone();
            let frame = foundry_contracts::context_v2::decode_frame(bytes)
                .map_err(|_| RuntimeError::Protocol("expected continuation acceptance"));
            frame.and_then(|frame| {
                frame
                    .check_accepted(scope, &identity, &receipt)
                    .map_err(|_| RuntimeError::Protocol("continuation acceptance mismatch"))?;
                *phase = Latch::Buffered {
                    attempt: identity.attempt_id.as_str().into(),
                    events: 0,
                    bytes: 0,
                };
                Ok(ContextOutput::Loading(frame))
            })
        }
        Latch::Buffered {
            attempt,
            events,
            bytes: used,
        } => {
            if *events >= BUFFER_EVENTS || bytes.len() > BUFFER_BYTES.saturating_sub(*used) {
                Err(RuntimeError::Protocol(
                    "continuation output buffer exceeded",
                ))
            } else {
                *events += 1;
                *used += bytes.len();
                ordinary(bytes, attempt, receipts, false)
            }
        }
        Latch::Admitted { attempt, capture } => ordinary(bytes, attempt, receipts, *capture),
        Latch::Failed => Err(RuntimeError::Protocol("continuation phase failed")),
    };
    if result.is_err() {
        *phase = Latch::Failed;
    }
    result
}
fn ordinary(
    bytes: &[u8],
    attempt: &str,
    receipts: &Mutex<HashMap<(String, String), String>>,
    capture: bool,
) -> Result<ContextOutput, RuntimeError> {
    let output = context::decode(bytes, Some(attempt), receipts);
    match &output {
        ContextOutput::Context(_) if capture => Ok(output),
        ContextOutput::Ordinary(WireEvent::Event(event)) => {
            let actual = match event {
                AdapterEvent::Text { attempt_id, .. }
                | AdapterEvent::ToolCall { attempt_id, .. }
                | AdapterEvent::MessageAccepted { attempt_id, .. }
                | AdapterEvent::Settled { attempt_id, .. }
                | AdapterEvent::Failed { attempt_id, .. } => attempt_id,
                AdapterEvent::Ready { .. } => {
                    return Err(RuntimeError::Protocol("repeated continuation readiness"));
                }
            };
            if actual != attempt {
                return Err(RuntimeError::Protocol("continuation attempt mismatch"));
            }
            Ok(output)
        }
        _ => Err(RuntimeError::Protocol("unexpected continuation output")),
    }
}
impl Process {
    pub(crate) fn prepare_continuation(
        &self,
        frame: &LoadingFrame,
    ) -> Result<LoadingCommand, RuntimeError> {
        let scope = self
            .loading_scope
            .as_ref()
            .ok_or(RuntimeError::Protocol("loading owner unavailable"))?;
        frame
            .check_scope(scope)
            .map_err(|_| RuntimeError::Protocol("continuation scope mismatch"))?;
        if frame.kind() != foundry_contracts::context_v2::FrameKind::Continue {
            return Err(RuntimeError::Protocol("expected continuation command"));
        }
        let mut bytes = foundry_contracts::context_v2::encode_frame(frame)
            .map_err(|_| RuntimeError::Protocol("invalid continuation command"))?;
        bytes.push(b'\n');
        let mut phase = self
            .latch
            .as_ref()
            .ok_or(RuntimeError::Invalid("missing loading latch"))?
            .lock()
            .map_err(|_| RuntimeError::Protocol("continuation phase unavailable"))?;
        if !matches!(*phase, Latch::Loading) {
            return Err(RuntimeError::Protocol("continuation already requested"));
        }
        *phase = Latch::AwaitAccepted {
            identity: frame.continuation_identity().unwrap().clone(),
            receipt: frame.receipt().unwrap().clone(),
        };
        Ok(LoadingCommand(InputFrame { bytes }))
    }
    pub(crate) fn admit_continuation(
        &mut self,
        attempt: &str,
        capture: bool,
    ) -> Result<(), RuntimeError> {
        let mut phase = self
            .latch
            .as_ref()
            .ok_or(RuntimeError::Invalid("missing loading latch"))?
            .lock()
            .map_err(|_| RuntimeError::Protocol("continuation phase unavailable"))?;
        if !matches!(&*phase, Latch::Buffered { attempt: held, .. } if held == attempt) {
            return Err(RuntimeError::Protocol("continuation not buffered"));
        }
        *phase = Latch::Admitted {
            attempt: attempt.into(),
            capture,
        };
        self.context_attempt = Some(attempt.into());
        self.loading_scope = None;
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "process_continuation_tests.rs"]
mod tests;
