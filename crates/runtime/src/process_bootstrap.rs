//! Private pre-Ready framing inside the existing process pipe workers.
use super::*;
use crate::bootstrap::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use foundry_contracts::context::ContextDigest;
use serde::{Deserialize, Serialize};
use std::{
    io,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    time::Instant,
};

struct State {
    deadline: Instant,
    ready: AtomicBool,
    written: AtomicU8,
}
pub(super) struct Prelude {
    bytes: Vec<u8>,
    state: Arc<State>,
}
pub(super) struct Gate {
    state: Arc<State>,
    identity: BootstrapIdentity,
    challenge: String,
    acknowledged: bool,
}
pub(super) fn prepare(spec: &AdapterSpec) -> Result<(Option<Prelude>, Option<Gate>), RuntimeError> {
    let started = Instant::now();
    let Some(packet) = spec.bootstrap.take()? else {
        return Ok((None, None));
    };
    let deadline = started
        .checked_add(spec.ready_timeout)
        .filter(|_| !spec.ready_timeout.is_zero())
        .ok_or(RuntimeError::Invalid(
            "invalid bootstrap readiness deadline",
        ))?;
    let challenge = uuid::Uuid::new_v4().to_string();
    #[derive(Serialize)]
    struct Frame<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        version: u32,
        selection_digest: ContextDigest,
        body_base64: String,
        challenge: &'a str,
    }
    let mut bytes = serde_json::to_vec(&Frame {
        kind: "adapter_bootstrap",
        version: BOOTSTRAP_VERSION,
        selection_digest: packet.identity.selection_digest,
        body_base64: STANDARD.encode(&packet.body),
        challenge: &challenge,
    })
    .map_err(|_| RuntimeError::Invalid("cannot encode private bootstrap"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BOOTSTRAP_FRAME_BYTES {
        return Err(RuntimeError::Invalid("private bootstrap frame limit"));
    }
    let state = Arc::new(State {
        deadline,
        ready: AtomicBool::new(false),
        written: AtomicU8::new(0),
    });
    Ok((
        Some(Prelude {
            bytes,
            state: Arc::clone(&state),
        }),
        Some(Gate {
            state,
            identity: packet.identity,
            challenge,
            acknowledged: false,
        }),
    ))
}
impl Gate {
    fn expired(&self) -> bool {
        !self.state.ready.load(Ordering::Acquire) && Instant::now() >= self.state.deadline
    }
    fn acknowledge(&mut self, bytes: &[u8], stop: &AtomicBool) -> Result<(), RuntimeError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Ack {
            #[serde(rename = "type")]
            kind: String,
            version: u32,
            selection_digest: ContextDigest,
            challenge: String,
        }
        let ack: Ack = serde_json::from_slice(bytes)
            .map_err(|_| RuntimeError::Protocol("invalid bootstrap acknowledgement"))?;
        if ack.kind != "adapter_configured"
            || ack.version != self.identity.schema_version
            || ack.selection_digest != self.identity.selection_digest
            || ack.challenge != self.challenge
        {
            return Err(RuntimeError::Protocol("bootstrap acknowledgement mismatch"));
        }
        loop {
            if self.expired() {
                return Err(RuntimeError::ReadyTimeout);
            }
            if pipe_workers::stopped(stop) {
                return Err(RuntimeError::Protocol("bootstrap stopped"));
            }
            match self.state.written.load(Ordering::Acquire) {
                1 => {
                    self.acknowledged = true;
                    return Ok(());
                }
                2 => return Err(RuntimeError::Protocol("bootstrap write failed")),
                _ => thread::sleep(POLL_INTERVAL),
            }
        }
    }
}

pub(super) fn input(
    stdin: impl Write,
    input: Receiver<InputFrame>,
    prelude: Option<Prelude>,
    errors: Output,
    stop: Arc<AtomicBool>,
) {
    let mut stdin = Pipe::new(stdin, Arc::clone(&stop));
    if let Some(prelude) = prelude {
        if Instant::now() >= prelude.state.deadline {
            prelude.state.written.store(2, Ordering::Release);
            errors.send(
                ContextOutput::Ordinary(WireEvent::Error(RuntimeError::ReadyTimeout)),
                &stop,
            );
            return;
        }
        let result = stdin.write_all(&prelude.bytes).and_then(|()| stdin.flush());
        prelude
            .state
            .written
            .store(if result.is_ok() { 1 } else { 2 }, Ordering::Release);
        if let Err(error) = result {
            errors.send(
                ContextOutput::Ordinary(WireEvent::Error(error.into())),
                &stop,
            );
            return;
        }
        let state = Arc::clone(&prelude.state);
        drop(prelude);
        while !state.ready.load(Ordering::Acquire) {
            if pipe_workers::stopped(&stop) || Instant::now() >= state.deadline {
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
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
}
struct DeadlineReader<R> {
    reader: R,
    state: Option<Arc<State>>,
}
impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self
            .state
            .as_ref()
            .is_some_and(|s| !s.ready.load(Ordering::Acquire) && Instant::now() >= s.deadline)
        {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        self.reader.read(bytes)
    }
}
pub(super) fn output(
    stdout: impl Read,
    mut identity: SpawnIdentity,
    mut gate: Option<Gate>,
    receipts: Arc<Mutex<HashMap<(String, String), String>>>,
    output: Output,
    cleanup: Option<Arc<cleanup::CaptureCleanup>>,
    stop: Arc<AtomicBool>,
) {
    let timed = DeadlineReader {
        reader: stdout,
        state: gate.as_ref().map(|g| Arc::clone(&g.state)),
    };
    let mut reader = BufReader::new(Pipe::new(timed, Arc::clone(&stop)));
    while !pipe_workers::stopped(&stop) {
        let limit = if gate.as_ref().is_some_and(|g| !g.acknowledged) {
            MAX_BOOTSTRAP_ACK_BYTES - 1
        } else {
            MAX_FRAME_BYTES
        };
        let frame = read_frame_limit(&mut reader, cleanup.as_deref(), limit);
        let item = if gate.as_ref().is_some_and(Gate::expired) {
            ContextOutput::Ordinary(WireEvent::Error(RuntimeError::ReadyTimeout))
        } else {
            match frame {
                Ok(Some(bytes)) => {
                    if let Some(gate) = gate.as_mut() {
                        if !gate.acknowledged {
                            match gate.acknowledge(&bytes, &stop) {
                                Ok(()) => continue,
                                Err(error) => {
                                    output.send(
                                        ContextOutput::Ordinary(WireEvent::Error(error)),
                                        &stop,
                                    );
                                    break;
                                }
                            }
                        }
                        if !gate.state.ready.load(Ordering::Acquire) {
                            let ready = serde_json::from_slice::<AdapterEvent>(&bytes);
                            if !matches!(
                                ready,
                                Ok(AdapterEvent::Ready {
                                    version: foundry_contracts::PROTOCOL_VERSION,
                                    ..
                                })
                            ) {
                                output.send(
                                    ContextOutput::Ordinary(WireEvent::Error(
                                        RuntimeError::Protocol("bootstrap requires readiness"),
                                    )),
                                    &stop,
                                );
                                break;
                            }
                        }
                    }
                    let item = identity.decode(&bytes, &receipts);
                    if matches!(
                        &item,
                        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
                    ) && let Some(gate) = &gate
                    {
                        gate.state.ready.store(true, Ordering::Release);
                    }
                    item
                }
                Ok(None) => ContextOutput::Ordinary(WireEvent::Closed),
                Err(error) => ContextOutput::Ordinary(WireEvent::Error(error)),
            }
        };
        let terminal = item.terminal();
        if !output.send(item, &stop) || terminal {
            break;
        }
    }
    if !reader.buffer().is_empty()
        && let Some(cleanup) = &cleanup
    {
        cleanup.discarded();
    }
}

#[cfg(test)]
#[path = "process_bootstrap_unit.rs"]
mod tests;
