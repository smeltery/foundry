use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 32;
enum Command {
    Text(Box<wire::Message>),
    State,
    Presence(wire::Presence),
}
struct Request {
    command: Command,
    result: oneshot::Sender<Result<()>>,
}
struct Control {
    connection: Connection,
    stopping: AtomicBool,
}
impl Control {
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.connection.close(0u32.into(), b"peer driver stopped");
    }
}
struct StopOnDrop(Arc<Control>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Sending finishes when bounded protocol output succeeds, not when a remote
/// receipt arrives. Persist the outbox before calling; settle only its own ACK.
#[derive(Clone)]
pub struct PeerSender {
    commands: mpsc::Sender<Request>,
    control: Arc<Control>,
}
impl PeerSender {
    pub(crate) fn diagnostic_stable_id(&self) -> usize {
        self.control.connection.stable_id()
    }
    async fn request(&self, command: Command) -> Result<()> {
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(PeerError::Closed);
        }
        let (result, reply) = oneshot::channel();
        self.commands
            .send(Request { command, result })
            .await
            .map_err(|_| PeerError::Closed)?;
        reply.await.map_err(|_| PeerError::Closed)?
    }
    pub async fn send_text(&self, message: wire::Message) -> Result<()> {
        self.request(Command::Text(Box::new(message))).await
    }
    pub async fn publish_state(&self) -> Result<()> {
        self.request(Command::State).await
    }
    pub async fn publish_presence(&self, presence: wire::Presence) -> Result<()> {
        self.request(Command::Presence(presence)).await
    }
    pub fn stop(&self) {
        self.control.stop();
    }
}

/// Events contain privileged presence/message identifiers; no Debug/Serialize.
pub enum DriverEvent {
    Warning(ProtocolMismatch),
    Peer(PeerEvent),
}
pub struct PeerDriver {
    events: mpsc::Receiver<DriverEvent>,
    control: Arc<Control>,
    task: Option<JoinHandle<Result<()>>>,
    // A receive-only owner can drop its sender without closing the command lane.
    _commands: mpsc::Sender<Request>,
}
impl PeerDriver {
    /// The reader owns its stream and emits only complete bound tokens. Outbound
    /// commands never cancel an in-flight partial frame. One session remains the
    /// sole writer/ACK owner and revalidates authority for every effect.
    pub fn start(mut session: PeerSession) -> Result<(PeerSender, Self)> {
        let mut reader = session.take_reader()?;
        let control = Arc::new(Control {
            connection: session.connection.clone(),
            stopping: AtomicBool::new(false),
        });
        let (commands, mut requests) = mpsc::channel::<Request>(COMMAND_CAPACITY);
        let (events, receiver) = mpsc::channel(EVENT_CAPACITY);
        let owned_control = control.clone();
        let task = tokio::spawn(async move {
            let _stop = StopOnDrop(owned_control.clone());
            if let Some(warning) = session.protocol_mismatch() {
                events
                    .try_send(DriverEvent::Warning(warning))
                    .map_err(|_| PeerError::Limit)?;
            }
            let (complete, mut frames) = mpsc::channel(2);
            let reading = tokio::spawn(async move {
                loop {
                    let frame = reader.next().await;
                    let failed = frame.is_err();
                    if complete.send(frame).await.is_err() || failed {
                        break;
                    }
                }
            });
            let result = loop {
                if owned_control.stopping.load(Ordering::Acquire) {
                    break Ok(());
                }
                tokio::select! {
                    request=requests.recv() => {
                        let Some(request)=request else { break Ok(()); };
                        let result=match request.command {
                            Command::Text(message)=>session.send_text(*message).await,
                            Command::State=>session.publish_state().await,
                            Command::Presence(presence)=>session.publish_presence(presence).await,
                        };
                        crate::daemon_outbox::diagnostic(format_args!("driver_command_result stable={} error={:?}",session.connection.stable_id(),result.as_ref().err()));
                        let failed=result.is_err();
                        let _=request.result.send(result);
                        if failed {
                            break if owned_control.stopping.load(Ordering::Acquire) { Ok(()) } else { Err(PeerError::Closed) };
                        }
                    }
                    frame=frames.recv() => {
                        let result=match frame {
                            Some(Ok(frame))=>session.receive_frame(frame).await,
                            Some(Err(error))=>Err(error),
                            None=>Err(PeerError::Closed),
                        };
                        crate::daemon_outbox::diagnostic(format_args!("driver_frame_result stable={} error={:?}",session.connection.stable_id(),result.as_ref().err()));
                        match result {
                            Ok(event)=>if events.try_send(DriverEvent::Peer(event)).is_err() { break Err(PeerError::Limit); },
                            Err(error)=>break if owned_control.stopping.load(Ordering::Acquire) { Ok(()) } else { Err(error) },
                        }
                    }
                }
            };
            crate::daemon_outbox::diagnostic(format_args!(
                "peer_driver_end stable={} error={:?}",
                session.connection.stable_id(),
                result.as_ref().err()
            ));
            owned_control.stop();
            drop(session);
            drop(frames);
            reading.await.map_err(|_| PeerError::Transport)?;
            result
        });
        Ok((
            PeerSender {
                commands: commands.clone(),
                control: control.clone(),
            },
            Self {
                events: receiver,
                control,
                task: Some(task),
                _commands: commands,
            },
        ))
    }
    pub async fn next_event(&mut self) -> Option<DriverEvent> {
        self.events.recv().await
    }
    /// Close transport, then join both owner and reader before returning. Dropping
    /// the handle closes transport too, but callers must await to prove draining.
    pub async fn shutdown(mut self) -> Result<()> {
        self.control.stop();
        self.task
            .take()
            .ok_or(PeerError::Closed)?
            .await
            .map_err(|_| PeerError::Transport)?
    }
}
impl Drop for PeerDriver {
    fn drop(&mut self) {
        self.control.stop();
    }
}
