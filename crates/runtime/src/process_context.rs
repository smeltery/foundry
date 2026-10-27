//! Ordered, explicitly opted-in private context transport. No phase/semantic
//! authorization is implied by routing; the future context supervisor owns it.
use super::cleanup::CaptureCleanup;
use super::{WireEvent, validate_ingress};
use crate::{RuntimeError, pipe_workers};
use foundry_contracts::{
    AdapterEvent,
    context::{self, ContextEvent},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc::SyncSender},
};

#[allow(dead_code)] // Consumed by the opt-in supervisor; never ordinary callbacks.
pub(crate) enum ContextOutput {
    Ordinary(WireEvent),
    Context(ContextEvent),
    Loading(foundry_contracts::context_v2::LoadingFrame),
}
impl ContextOutput {
    pub(super) fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Ordinary(WireEvent::Error(_) | WireEvent::Closed)
        )
    }
}

#[derive(Clone)]
pub(super) enum Output {
    Legacy(SyncSender<WireEvent>),
    Context(SyncSender<ContextOutput>, Arc<CaptureCleanup>),
}
impl Output {
    pub fn send(&self, item: ContextOutput, stop: &AtomicBool) -> bool {
        match self {
            Self::Context(sender, cleanup) => {
                cleanup.observe(&item);
                let data = matches!(
                    item,
                    ContextOutput::Context(_)
                        | ContextOutput::Loading(_)
                        | ContextOutput::Ordinary(WireEvent::Event(_))
                );
                let sent = pipe_workers::send(sender, item, stop);
                if !sent && data {
                    cleanup.discarded();
                }
                sent
            }
            Self::Legacy(sender) => {
                let event = match item {
                    ContextOutput::Ordinary(event) => event,
                    ContextOutput::Context(_) | ContextOutput::Loading(_) => {
                        WireEvent::Error(RuntimeError::Protocol("unsolicited context frame"))
                    }
                };
                pipe_workers::send(sender, event, stop)
            }
        }
    }
}

pub(super) fn decode(
    bytes: &[u8],
    attempt: Option<&str>,
    receipts: &Mutex<HashMap<(String, String), String>>,
) -> ContextOutput {
    let parsed = (|| {
        let context_frame = context::is_context_frame(bytes)
            .map_err(|_| RuntimeError::Protocol("invalid JSONL event"))?;
        if context_frame {
            let expected = attempt.ok_or(RuntimeError::Protocol("unsolicited context frame"))?;
            let event = context::decode_event(bytes)
                .map_err(|_| RuntimeError::Protocol("invalid context frame"))?;
            if event.attempt_id().as_str() != expected {
                return Err(RuntimeError::Protocol(
                    "context frame belongs to another attempt",
                ));
            }
            Ok(ContextOutput::Context(event))
        } else {
            let event: AdapterEvent = serde_json::from_slice(bytes)
                .map_err(|_| RuntimeError::Protocol("invalid JSONL event"))?;
            Ok(ContextOutput::Ordinary(validate_ingress(event, receipts)))
        }
    })();
    parsed.unwrap_or_else(|error| ContextOutput::Ordinary(WireEvent::Error(error)))
}
