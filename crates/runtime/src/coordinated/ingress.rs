//! One ordered input seam for legacy execution and trusted capture supervision.
use super::{
    TrustedToolPolicy,
    capture_owner::CaptureOwner,
    startup::{Startup, StartupPlan},
};
use crate::{
    AdapterSpec, RuntimeError,
    process::{ContextOutput, Process, WireEvent},
};
use foundry_contracts::Session;
use std::sync::mpsc::{Receiver, TryRecvError};

pub(super) struct ExecutionOptions {
    pub mode: ExecutionMode,
    pub policy: Option<TrustedToolPolicy>,
}

pub(super) enum ExecutionMode {
    Legacy,
    #[allow(dead_code)] // Transport-only test mode; no production caller or phase authority.
    CaptureReserved,
    CaptureStartup(StartupPlan, CaptureOwner),
}

pub(super) enum Ingress {
    Legacy,
    Context(Receiver<ContextOutput>),
}

impl ExecutionMode {
    pub fn spawn(
        self,
        spec: &AdapterSpec,
        session: &Session,
    ) -> Result<(Process, Ingress, Option<Startup>), RuntimeError> {
        match self {
            Self::Legacy => Ok((Process::spawn(spec, session)?, Ingress::Legacy, None)),
            Self::CaptureReserved => {
                let (process, events) = Process::spawn_context(spec, session)?;
                Ok((process, Ingress::Context(events), None))
            }
            Self::CaptureStartup(plan, owner) => {
                let startup = Startup::new(
                    plan,
                    owner,
                    session.active_attempt_id.as_deref().unwrap_or(""),
                )?;
                let (process, events) = Process::spawn_context(spec, session)?;
                Ok((process, Ingress::Context(events), Some(startup)))
            }
        }
    }
}

impl Ingress {
    pub fn try_recv(&self, legacy: &Receiver<WireEvent>) -> Result<ContextOutput, TryRecvError> {
        match self {
            Self::Legacy => legacy.try_recv().map(ContextOutput::Ordinary),
            Self::Context(events) => events.try_recv(),
        }
    }

    /// Reject context transport without a capture controller; never fall back to Start.
    pub fn require_execution_support(&self) -> Result<(), RuntimeError> {
        match self {
            Self::Legacy => Ok(()),
            Self::Context(_) => Err(RuntimeError::Invalid("capture supervision is unavailable")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_contracts::{AdapterEvent, context::*};
    use std::sync::mpsc;

    #[test]
    fn private_stream_preserves_order_and_never_reads_legacy_queue() {
        let (legacy_tx, legacy_rx) = mpsc::sync_channel(1);
        legacy_tx.send(WireEvent::Closed).unwrap();
        let (sender, receiver) = mpsc::sync_channel(2);
        let ingress = Ingress::Context(receiver);
        sender
            .send(ContextOutput::Ordinary(WireEvent::Event(
                AdapterEvent::Ready {
                    version: 1,
                    capabilities: vec![],
                },
            )))
            .ok()
            .unwrap();
        sender
            .send(ContextOutput::Context(ContextEvent::Failed {
                attempt_id: ContextId::new("attempt").unwrap(),
                operation_id: ContextId::new("capture").unwrap(),
                code: ContextFailure::Busy,
            }))
            .ok()
            .unwrap();
        assert!(matches!(
            ingress.try_recv(&legacy_rx),
            Ok(ContextOutput::Ordinary(WireEvent::Event(
                AdapterEvent::Ready { .. }
            )))
        ));
        assert!(matches!(
            ingress.try_recv(&legacy_rx),
            Ok(ContextOutput::Context(ContextEvent::Failed { .. }))
        ));
        assert!(matches!(
            ingress.try_recv(&legacy_rx),
            Err(TryRecvError::Empty)
        ));
        drop(sender);
        assert!(matches!(
            ingress.try_recv(&legacy_rx),
            Err(TryRecvError::Disconnected)
        ));
        assert!(matches!(legacy_rx.try_recv(), Ok(WireEvent::Closed)));
        assert!(matches!(
            ingress.require_execution_support(),
            Err(RuntimeError::Invalid(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn reserved_capture_mode_cleans_up_without_start_or_persistence() {
        use super::super::{RuntimePersistence, engine};
        use foundry_contracts::Lifecycle;
        let marker = std::env::temp_dir().join(format!("f5y-no-start-{}", uuid::Uuid::new_v4()));
        let python = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
        spec.args = ["-u", "-c",
            "import json,sys,pathlib; print(json.dumps({'type':'ready','version':1}),flush=True); data=sys.stdin.readline(); pathlib.Path(sys.argv[1]).write_text(data) if data else None",
            marker.to_str().unwrap()].into_iter().map(Into::into).collect();
        let session = Session {
            id: "s".into(),
            prompt: "fixture".into(),
            directory: std::env::temp_dir().to_string_lossy().into(),
            model: None,
            lifecycle: Lifecycle::Running,
            outcome: None,
            result: None,
            active_attempt_id: Some("attempt".into()),
            version: 1,
        };
        let (_cancel, cancellation) = mpsc::channel();
        let (_messages, messages) = mpsc::channel();
        let (requests, received) = mpsc::sync_channel(2);
        let (_replies, replies) = mpsc::sync_channel(2);
        let result = engine::run(
            &spec,
            &session,
            "attempt",
            &cancellation,
            &messages,
            RuntimePersistence { requests, replies },
            ExecutionOptions {
                mode: ExecutionMode::CaptureReserved,
                policy: None,
            },
        );
        let started = marker.exists();
        if started {
            std::fs::remove_file(&marker).unwrap();
        }
        assert!(matches!(
            result,
            Err(RuntimeError::Invalid("capture supervision is unavailable"))
        ));
        assert!(!started);
        assert!(matches!(
            received.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
}
