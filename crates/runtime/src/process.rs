use crate::{
    AdapterSpec, RuntimeError,
    pipe_workers::{self, POLL_INTERVAL, Pipe, Workers},
};
use foundry_contracts::{AdapterCommand, AdapterEvent, MAX_FRAME_BYTES, Session};
#[path = "process_cleanup.rs"]
mod cleanup;
#[path = "process_context.rs"]
mod context;
#[path = "process_continuation.rs"]
mod continuation;
#[path = "process_loading.rs"]
mod loading;
#[path = "process_messages.rs"]
mod messages;
pub(crate) use context::ContextOutput;
use context::Output;
use loading::SpawnIdentity;
pub(crate) use loading::{LoadingCommand, LoadingProcessSpec, LoadingSendError};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub(crate) enum WireEvent {
    Event(AdapterEvent),
    Error(RuntimeError),
    Closed,
}

struct InputFrame {
    bytes: Vec<u8>,
}

pub(crate) struct Process {
    child: Child,
    commands: Option<SyncSender<InputFrame>>,
    workers: Workers,
    pub events: Receiver<WireEvent>,
    stopped: bool,
    context_attempt: Option<String>,
    loading_scope: Option<foundry_contracts::context_v2::LoadingScope>,
    latch: Option<Arc<Mutex<continuation::Latch>>>,
    receipts: Arc<Mutex<HashMap<(String, String), String>>>,
    cleanup: Option<Arc<cleanup::CaptureCleanup>>,
}

impl Process {
    pub(crate) fn process_id(&self) -> u32 {
        self.child.id()
    }
    pub fn spawn(spec: &AdapterSpec, session: &Session) -> Result<Self, RuntimeError> {
        Self::spawn_inner(
            spec,
            std::path::Path::new(&session.directory),
            SpawnIdentity::Legacy,
            None,
        )
    }

    /// Explicit transport opt-in, not a semantic validation or restore capability.
    pub fn spawn_context(
        spec: &AdapterSpec,
        session: &Session,
    ) -> Result<(Self, Receiver<ContextOutput>), RuntimeError> {
        let (output, events) = sync_channel(2);
        let attempt = session
            .active_attempt_id
            .as_ref()
            .ok_or(RuntimeError::Invalid(
                "context transport requires an active attempt",
            ))?;
        foundry_contracts::context::ContextId::new(attempt.clone())
            .map_err(|_| RuntimeError::Invalid("invalid context attempt"))?;
        let process = Self::spawn_inner(
            spec,
            std::path::Path::new(&session.directory),
            SpawnIdentity::FixedAttempt(attempt.clone()),
            Some(output),
        )?;
        Ok((process, events))
    }

    fn spawn_inner(
        spec: &AdapterSpec,
        directory: &std::path::Path,
        mut identity: SpawnIdentity,
        context: Option<SyncSender<ContextOutput>>,
    ) -> Result<Self, RuntimeError> {
        let context_attempt = identity.attempt().map(str::to_owned);
        let loading_scope = identity.loading_scope().cloned();
        let latch = identity.latch();
        let capacity = if context.is_some() { 2 } else { 16 };
        if !cfg!(unix) {
            return Err(RuntimeError::UnsupportedPlatform);
        }
        let executable = spec.executable.canonicalize()?;
        let mut command = Command::new(executable);
        command.args(&spec.args).current_dir(directory).env_clear();
        // No HOME, credential, model-provider or user configuration variables.
        // Callers needing secrets must add a separately reviewed credential channel.
        for key in ["PATH", "LANG", "LC_ALL", "TZ"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (commands, input) = sync_channel::<InputFrame>(capacity);
        let receipts = Arc::new(Mutex::new(HashMap::new()));
        let reader_receipts = Arc::clone(&receipts);
        let (output, events) = sync_channel(capacity);
        let cleanup = context
            .as_ref()
            .map(|_| Arc::new(cleanup::CaptureCleanup::default()));
        let output = context.map_or_else(
            || Output::Legacy(output),
            |sender| Output::Context(sender, Arc::clone(cleanup.as_ref().unwrap())),
        );
        let reader_cleanup = cleanup.clone();
        let mut process = Self {
            child,
            commands: Some(commands),
            workers: Workers::default(),
            events,
            stopped: false,
            context_attempt,
            loading_scope,
            latch,
            receipts,
            cleanup,
        };
        pipe_workers::nonblocking(&stdin)?;
        pipe_workers::nonblocking(&stdout)?;
        pipe_workers::nonblocking(&stderr)?;
        let errors = output.clone();
        process.workers.spawn("adapter-input", move |stop| {
            let mut stdin = Pipe::new(stdin, Arc::clone(&stop));
            while !pipe_workers::stopped(&stop) {
                let frame = match input.recv_timeout(POLL_INTERVAL) {
                    Ok(frame) => frame,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if let Err(error) = stdin.write_all(&frame.bytes).and_then(|()| stdin.flush()) {
                    errors.send(
                        ContextOutput::Ordinary(WireEvent::Error(error.into())),
                        &stop,
                    );
                    break;
                }
            }
        })?;
        process.workers.spawn("adapter-output", move |stop| {
            let mut reader = BufReader::new(Pipe::new(stdout, Arc::clone(&stop)));
            while !pipe_workers::stopped(&stop) {
                let item = match read_frame(&mut reader, reader_cleanup.as_deref()) {
                    Ok(Some(bytes)) => identity.decode(&bytes, &reader_receipts),
                    Ok(None) => ContextOutput::Ordinary(WireEvent::Closed),
                    Err(error) => ContextOutput::Ordinary(WireEvent::Error(error)),
                };
                let terminal = item.terminal();
                if !output.send(item, &stop) || terminal {
                    break;
                }
            }
            if !reader.buffer().is_empty()
                && let Some(cleanup) = &reader_cleanup
            {
                cleanup.discarded();
            }
        })?;
        process.workers.spawn("adapter-stderr", move |stop| {
            // Fixed-memory drain; secret-bearing stderr is never forwarded.
            let mut stderr = Pipe::new(stderr, Arc::clone(&stop));
            let mut buffer = [0_u8; 8192];
            while !pipe_workers::stopped(&stop) {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })?;
        Ok(process)
    }

    /// Correlation receipts and phase ordering must be checked by the context owner.
    #[allow(dead_code)]
    pub fn send_context(
        &self,
        command: &foundry_contracts::context::ContextCommand,
    ) -> Result<(), RuntimeError> {
        if self.context_attempt.as_deref() != Some(command.attempt_id().as_str()) {
            return Err(RuntimeError::Protocol(
                "context command is not authorized for this transport",
            ));
        }
        let mut bytes = foundry_contracts::context::encode_command(command)
            .map_err(|_| RuntimeError::Protocol("invalid context command"))?;
        bytes.push(b'\n');
        self.commands
            .as_ref()
            .ok_or(RuntimeError::CommandQueue)?
            .try_send(InputFrame { bytes })
            .map_err(|_| RuntimeError::CommandQueue)
    }

    #[cfg(unix)]
    fn reap_exiting_group(&mut self) -> Result<bool, RuntimeError> {
        let group = -(self.child.id() as i32);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        loop {
            if self.child.try_wait()?.is_some() {
                // Reaping releases the PID reservation: never signal this group again.
                self.stopped = true;
                let probe = unsafe { libc::kill(group, 0) };
                return Ok(probe != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(cleanup) = &self.cleanup {
            cleanup.begin_shutdown();
        }
        self.workers.request_stop();
        self.commands.take();
        let child_result = self.stop_child();
        // Even a failed kill/reap must close our pipe ends and join every worker.
        let workers_result = self.workers.join();
        child_result.and(workers_result)
    }

    fn stop_child(&mut self) -> Result<(), RuntimeError> {
        if self.stopped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // The unreaped direct child reserves this PID, which is also the PGID.
            // Kill descendants even when the leader has already exited.
            let result = unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    // An exiting leader may briefly reject group signals before becoming
                    // waitable. Only accept that race after reaping and proving the group gone.
                    if error.raw_os_error() == Some(libc::EPERM) && self.reap_exiting_group()? {
                        return Ok(());
                    }
                    return Err(error.into());
                }
            }
        }
        self.child.wait()?;
        self.stopped = true;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn read_frame(
    reader: &mut impl BufRead,
    cleanup: Option<&cleanup::CaptureCleanup>,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    let mut frame = Vec::new();
    loop {
        let buffer = reader.fill_buf().inspect_err(|_| {
            if !frame.is_empty()
                && let Some(cleanup) = cleanup
            {
                cleanup.discarded();
            }
        })?;
        if buffer.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(RuntimeError::Protocol("unterminated JSONL frame"))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(buffer.len());
        if frame.len() + length > MAX_FRAME_BYTES {
            return Err(RuntimeError::FrameTooLarge);
        }
        frame.extend_from_slice(&buffer[..length]);
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(frame));
        }
    }
}

// The receipt is serialized after the text. Matching it establishes knowledge of
// that payload tail, not facade execution or the writer thread's scheduling.
fn validate_ingress(
    event: AdapterEvent,
    receipts: &Mutex<HashMap<(String, String), String>>,
) -> WireEvent {
    if let AdapterEvent::MessageAccepted {
        attempt_id,
        message_id,
        receipt,
    } = &event
    {
        let Ok(receipts) = receipts.lock() else {
            return WireEvent::Error(RuntimeError::Protocol("message tracking unavailable"));
        };
        if receipts.get(&(attempt_id.clone(), message_id.clone())) != Some(receipt) {
            return WireEvent::Error(RuntimeError::Protocol("acknowledgment receipt mismatch"));
        }
    }
    WireEvent::Event(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_is_serialized_after_payload() {
        let command = AdapterCommand::Message {
            attempt_id: "a".into(),
            message_id: "m".into(),
            text: "payload".into(),
            receipt: "challenge".into(),
        };
        let bytes = serde_json::to_string(&command).unwrap();
        assert!(bytes.ends_with(r#""text":"payload","receipt":"challenge"}"#));
    }

    #[test]
    fn ingress_does_not_depend_on_writer_completion_bookkeeping() {
        let receipts = Mutex::new(HashMap::new());
        let ack = AdapterEvent::MessageAccepted {
            attempt_id: "a".into(),
            message_id: "m".into(),
            receipt: "fresh".into(),
        };
        let queued = validate_ingress(ack.clone(), &receipts);
        receipts
            .lock()
            .unwrap()
            .insert(("a".into(), "m".into()), "fresh".into());
        assert!(matches!(queued, WireEvent::Error(_)));
        // Registration precedes the write; a genuine fast receipt needs no
        // writer-completed flag and therefore cannot fail due to scheduling.
        assert!(matches!(
            validate_ingress(ack, &receipts),
            WireEvent::Event(_)
        ));
        let forged = AdapterEvent::MessageAccepted {
            attempt_id: "a".into(),
            message_id: "m".into(),
            receipt: "guessed".into(),
        };
        assert!(matches!(
            validate_ingress(forged, &receipts),
            WireEvent::Error(_)
        ));
    }
}

#[cfg(all(test, unix))]
#[path = "process_tests.rs"]
mod worker_tests;

#[cfg(all(test, unix))]
#[path = "process_context_tests.rs"]
mod context_tests;
