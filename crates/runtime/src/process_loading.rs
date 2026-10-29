//! Loading transport; the coordinated loading controller owns phase authority.
use super::*;
use foundry_contracts::{
    PROTOCOL_VERSION,
    context_v2::{self, FrameKind, LoadingFrame, LoadingScope},
};
use std::path::Path;

pub(crate) struct LoadingCommand(pub(super) InputFrame);
pub(crate) enum LoadingSendError {
    Full(LoadingCommand),
    Disconnected,
}

pub(crate) struct LoadingProcessSpec<'a> {
    pub directory: &'a Path,
    pub scope: LoadingScope,
}

pub(super) enum SpawnIdentity {
    Legacy,
    FixedAttempt(String),
    Loading {
        scope: LoadingScope,
        ready: bool,
        latch: Arc<Mutex<super::continuation::Latch>>,
    },
}
impl SpawnIdentity {
    pub fn latch(&self) -> Option<Arc<Mutex<super::continuation::Latch>>> {
        if let Self::Loading { latch, .. } = self {
            Some(Arc::clone(latch))
        } else {
            None
        }
    }
    pub fn attempt(&self) -> Option<&str> {
        if let Self::FixedAttempt(attempt) = self {
            Some(attempt)
        } else {
            None
        }
    }
    pub fn loading_scope(&self) -> Option<&LoadingScope> {
        if let Self::Loading { scope, .. } = self {
            Some(scope)
        } else {
            None
        }
    }
    pub fn decode(
        &mut self,
        bytes: &[u8],
        receipts: &Mutex<HashMap<(String, String), String>>,
    ) -> ContextOutput {
        if let Self::Loading {
            scope,
            ready,
            latch,
        } = self
        {
            let result = super::continuation::decode(bytes, scope, ready, latch, receipts);
            result.unwrap_or_else(|error| ContextOutput::Ordinary(WireEvent::Error(error)))
        } else {
            context::decode(bytes, self.attempt(), receipts)
        }
    }
}

pub(super) fn decode_loading(
    bytes: &[u8],
    scope: &LoadingScope,
    ready: &mut bool,
) -> Result<ContextOutput, RuntimeError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(RuntimeError::FrameTooLarge);
    }
    #[derive(serde::Deserialize)]
    struct Tag {
        #[serde(rename = "type")]
        kind: String,
    }
    let tag: Tag = serde_json::from_slice(bytes)
        .map_err(|_| RuntimeError::Protocol("invalid loading frame"))?;
    if tag.kind == "ready" {
        let AdapterEvent::Ready {
            version,
            capabilities,
        } = serde_json::from_slice(bytes)
            .map_err(|_| RuntimeError::Protocol("invalid loading readiness"))?
        else {
            return Err(RuntimeError::Protocol("invalid loading readiness"));
        };
        if *ready || version != PROTOCOL_VERSION {
            return Err(RuntimeError::Protocol(
                "duplicate or incompatible loading readiness",
            ));
        }
        if !capabilities
            .iter()
            .any(|value| value == context_v2::RESTORE_CAPABILITY)
        {
            return Err(RuntimeError::Protocol(
                "loading restoration capability missing",
            ));
        }
        *ready = true;
        // Ready stays on the private stream; there is no admitted attempt or event sink.
        return Ok(ContextOutput::Ordinary(WireEvent::Event(
            AdapterEvent::Ready {
                version,
                capabilities,
            },
        )));
    }
    if !*ready {
        return Err(RuntimeError::Protocol("loading control before readiness"));
    }
    if matches!(
        tag.kind.as_str(),
        "context_observed_v1" | "context_observation_rejected_v1"
    ) {
        let frame = foundry_contracts::context_observation::decode_response(bytes)
            .map_err(|_| RuntimeError::Protocol("invalid observation response"))?;
        if &frame.binding().scope != scope {
            return Err(RuntimeError::Protocol("observation scope mismatch"));
        }
        return Ok(ContextOutput::Observation(frame));
    }
    let frame = context_v2::decode_frame(bytes)
        .map_err(|_| RuntimeError::Protocol("invalid loading control"))?;
    frame
        .check_scope(scope)
        .map_err(|_| RuntimeError::Protocol("loading scope mismatch"))?;
    if !matches!(frame.kind(), FrameKind::Prepared | FrameKind::Restored) {
        return Err(RuntimeError::Protocol("unsupported loading response kind"));
    }
    Ok(ContextOutput::Loading(frame))
}

impl Process {
    /// Private loading transport shared by the controller and future continuation owner.
    pub(crate) fn spawn_loading(
        spec: &AdapterSpec,
        loading: LoadingProcessSpec<'_>,
    ) -> Result<(Self, Receiver<ContextOutput>), RuntimeError> {
        if !loading.directory.is_absolute() || !loading.directory.is_dir() {
            return Err(RuntimeError::Invalid(
                "loading requires an absolute directory",
            ));
        }
        let (output, events) = sync_channel(2);
        let process = Self::spawn_inner(
            spec,
            loading.directory,
            SpawnIdentity::Loading {
                scope: loading.scope,
                ready: false,
                latch: Arc::new(Mutex::new(super::continuation::Latch::Loading)),
            },
            Some(output),
        )?;
        Ok((process, events))
    }
    pub(crate) fn prepare_loading(
        &self,
        frame: &LoadingFrame,
    ) -> Result<LoadingCommand, RuntimeError> {
        let scope = self
            .loading_scope
            .as_ref()
            .ok_or(RuntimeError::Protocol("loading transport unavailable"))?;
        frame
            .check_scope(scope)
            .map_err(|_| RuntimeError::Protocol("loading command scope mismatch"))?;
        if !matches!(frame.kind(), FrameKind::Begin | FrameKind::Payload) {
            return Err(RuntimeError::Protocol("unsupported loading command kind"));
        }
        let mut bytes = context_v2::encode_frame(frame)
            .map_err(|_| RuntimeError::Protocol("invalid loading command"))?;
        bytes.push(b'\n');
        Ok(LoadingCommand(InputFrame { bytes }))
    }
    pub(crate) fn prepare_observation(
        &self,
        frame: &foundry_contracts::context_observation::ObservationFrame,
    ) -> Result<LoadingCommand, RuntimeError> {
        if self.loading_scope.as_ref() != Some(&frame.binding().scope) {
            return Err(RuntimeError::Protocol("observation command scope mismatch"));
        }
        let mut bytes = foundry_contracts::context_observation::encode(frame)
            .map_err(|_| RuntimeError::Protocol("invalid observation request"))?;
        foundry_contracts::context_observation::decode_request(&bytes)
            .map_err(|_| RuntimeError::Protocol("invalid observation command direction"))?;
        bytes.push(b'\n');
        Ok(LoadingCommand(InputFrame { bytes }))
    }
    pub(crate) fn try_send_loading(&self, command: LoadingCommand) -> Result<(), LoadingSendError> {
        use std::sync::mpsc::TrySendError;
        let sender = self
            .commands
            .as_ref()
            .ok_or(LoadingSendError::Disconnected)?;
        match sender.try_send(command.0) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(frame)) => Err(LoadingSendError::Full(LoadingCommand(frame))),
            Err(TrySendError::Disconnected(_)) => Err(LoadingSendError::Disconnected),
        }
    }
    /// Transport-only helper retained for transport fixtures.
    #[cfg(test)]
    pub(crate) fn send_loading(&self, frame: &LoadingFrame) -> Result<(), RuntimeError> {
        let command = self.prepare_loading(frame)?;
        self.try_send_loading(command)
            .map_err(|_| RuntimeError::CommandQueue)
    }
}

#[cfg(all(test, unix))]
#[path = "process_loading_tests.rs"]
mod tests;
