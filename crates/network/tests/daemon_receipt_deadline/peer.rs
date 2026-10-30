use super::{DEADLINE, admission::Admitted};
use foundry_network::{PeerFrame, read_frame, write_frame};
use iroh::{
    EndpointAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

pub enum Event {
    Message {
        payload: Vec<u8>,
        connection: usize,
        duplicate: bool,
    },
    Closed {
        connection: usize,
        elapsed: Duration,
        heartbeats: usize,
    },
    ReceiptLost {
        connection: usize,
    },
    AckWritten {
        connection: usize,
    },
}
pub struct Controlled {
    pub address: EndpointAddr,
    events: mpsc::Receiver<Event>,
    ack: mpsc::Sender<()>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}
impl Controlled {
    pub fn start(admitted: Admitted) -> Self {
        Self::start_with_fault(admitted, false)
    }
    pub fn start_with_lost_replay_receipt(admitted: Admitted) -> Self {
        Self::start_with_fault(admitted, true)
    }
    fn start_with_fault(admitted: Admitted, lose_replay: bool) -> Self {
        let address = admitted.endpoint.address();
        let (events, receive) = mpsc::channel(8);
        let (ack, permission) = mpsc::channel(1);
        let (stop, cancel) = watch::channel(false);
        let task = tokio::spawn(run(admitted, events, permission, cancel, lose_replay));
        Self {
            address,
            events: receive,
            ack,
            stop,
            task: Some(task),
        }
    }
    pub async fn event(&mut self) -> Event {
        tokio::time::timeout(DEADLINE, self.events.recv())
            .await
            .expect("controlled peer observation deadline")
            .expect("controlled peer stopped early")
    }
    pub async fn acknowledge_replay(&self) {
        assert!(self.ack.send(()).await.is_ok());
    }
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Controlled {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
struct Reading(JoinHandle<()>);
impl Drop for Reading {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(super) fn diagnostic(value: std::fmt::Arguments<'_>) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let Some(path) = std::env::var_os("FOUNDRY_ACK_DIAGNOSTIC") else {
        return;
    };
    let line = format!(
        "fixture pid={} ms={} {}\n",
        std::process::id(),
        super::admission::now(),
        value
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        if file.metadata().is_ok_and(|m| m.len() < 8 * 1024 * 1024)
            && file.write_all(line.as_bytes()).is_err()
        {
            eprintln!("diagnostic fixture write failed");
        }
    } else {
        eprintln!("diagnostic fixture open failed");
    }
}
async fn handshake(
    admitted: &mut Admitted,
    connection: &Connection,
) -> foundry_network::Result<(SendStream, RecvStream)> {
    assert_eq!(connection.remote_id(), admitted.remote);
    assert!(connection.alpn() == admitted.profile.team_alpn);
    let (mut send, mut receive) = connection
        .accept_bi()
        .await
        .map_err(|_| foundry_network::NetworkError::Closed)?;
    write_frame(
        &mut send,
        &PeerFrame::Hello {
            team: admitted.team.clone(),
            proto: admitted.profile.protocol,
        },
        DEADLINE,
    )
    .await?;
    match read_frame(&mut receive, DEADLINE).await? {
        Some(PeerFrame::Hello { team, proto }) => {
            assert!(team == admitted.team && proto == admitted.profile.protocol)
        }
        None => return Err(foundry_network::NetworkError::Closed),
        _ => panic!("actual TEAM hello required"),
    }
    write_frame(&mut send, &admitted.state_frame(), DEADLINE).await?;
    match read_frame(&mut receive, DEADLINE).await? {
        Some(PeerFrame::State { state }) => admitted.merge(*state),
        None => return Err(foundry_network::NetworkError::Closed),
        _ => panic!("actual TEAM state exchange required"),
    }
    Ok((send, receive))
}
fn observe(events: &mpsc::Sender<Event>, event: Event) {
    assert!(
        events.try_send(event).is_ok(),
        "required fixture observation unavailable"
    );
}
struct Receipts {
    // One permission survives transport retirement; writes remain per connection.
    enabled: bool,
    lose_next: bool,
    written: bool,
}
impl Receipts {
    // True retires only this transport; no successful write is reported on loss.
    async fn acknowledge(
        &mut self,
        connection: &Connection,
        send: &mut SendStream,
        id: &str,
        events: &mpsc::Sender<Event>,
        written_here: &mut bool,
    ) -> bool {
        if !self.enabled || *written_here {
            return false;
        }
        if self.lose_next {
            self.lose_next = false;
            // Deliberate fixture-only loss: suppress this receipt and
            // retire its actual transport after the caller grants permission.
            connection.close(0u32.into(), b"fixture lost replay receipt");
            observe(
                events,
                Event::ReceiptLost {
                    connection: connection.stable_id(),
                },
            );
            return true;
        }
        let receipt = PeerFrame::Ack {
            id: id.into(),
            state: "queued".into(),
            note: String::new(),
        };
        diagnostic(format_args!("ack_start stable={}", connection.stable_id()));
        if write_frame(send, &receipt, DEADLINE).await.is_err() {
            connection.close(0u32.into(), b"fixture receipt output failed");
            return true;
        }
        diagnostic(format_args!(
            "ack_written stable={}",
            connection.stable_id()
        ));
        *written_here = true;
        if !self.written {
            self.written = true;
            observe(
                events,
                Event::AckWritten {
                    connection: connection.stable_id(),
                },
            );
        }
        false
    }
}
async fn run(
    mut admitted: Admitted,
    events: mpsc::Sender<Event>,
    mut ack: mpsc::Receiver<()>,
    mut stop: watch::Receiver<bool>,
    lose_replay: bool,
) {
    let mut first_saved = false;
    let mut receipts = Receipts {
        enabled: false,
        lose_next: lose_replay,
        written: false,
    };
    let mut replay_reported = false;
    // Retain closed handles too, so stable IDs cannot be confused by reuse.
    let mut retained = Vec::new();
    for _ in 0..8 {
        if *stop.borrow() {
            break;
        }
        let accepted = tokio::select! {
            _=stop.changed()=>break,
            accepted=admitted.endpoint.accept_routed(DEADLINE)=>accepted.unwrap(),
        };
        assert!(matches!(accepted.channel(), foundry_network::Channel::Team));
        let connection = admitted.endpoint.into_connection(accepted).unwrap();
        let (mut send, mut receive) = match handshake(&mut admitted, &connection).await {
            Ok(streams) => streams,
            Err(_) => continue, // A concurrently superseded pre-send dial owns no receipt.
        };
        retained.push(connection.clone());
        let (frames, mut complete) = mpsc::channel(2);
        let reading = Reading(tokio::spawn(async move {
            loop {
                let frame = read_frame(&mut receive, DEADLINE).await;
                let done = !matches!(frame, Ok(Some(_)));
                if frames.send(frame).await.is_err() || done {
                    break;
                }
            }
        }));
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut message_at = None;
        let mut message_id = None;
        let mut heartbeats = 0;
        let mut replay = false;
        let mut written_here = false;
        let stopped = loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break true,
                _ = connection.closed() => break false,
                frame = complete.recv() => match frame {
                    Some(Ok(Some(PeerFrame::State { state }))) => admitted.merge(*state),
                    Some(Ok(Some(PeerFrame::Presence { .. }))) => {},
                    Some(Ok(Some(PeerFrame::Msg { msg }))) => {
                        assert!(message_at.is_none(),
                            "same connection retransmitted instead of retiring its old owner");
                        let (inserted, payload) = admitted.persist(&msg);
                        replay = first_saved;
                        diagnostic(format_args!("message stable={} replay={}",connection.stable_id(),replay));
                        let expected = if replay {
                            foundry_network::mail_store::Insert::Duplicate
                        } else {
                            foundry_network::mail_store::Insert::Created
                        };
                        assert!(inserted == expected);
                        first_saved = true;
                        message_at = Some(Instant::now());
                        message_id = Some(msg.id);
                        if !replay || !replay_reported {
                            replay_reported |= replay;
                            observe(&events, Event::Message {
                                payload,
                                connection: connection.stable_id(),
                                duplicate: replay,
                            });
                        }
                        if replay && receipts.acknowledge(&connection, &mut send, message_id.as_ref().unwrap(), &events, &mut written_here).await { break false; }
                        // Deliberately NO ACK on the first connection. Presence
                        // output and the dedicated inbound reader remain live.
                    }
                    Some(Ok(Some(_))) => panic!("unexpected post-handshake frame"),
                    _ => {
                        tokio::time::timeout(DEADLINE, connection.closed()).await
                            .expect("stream closure alone is not connection retirement");
                        break false;
                    }
                },
                permission = ack.recv(), if replay && !receipts.enabled => {
                    assert!(permission.is_some());
                    receipts.enabled = true;
                    if receipts.acknowledge(&connection, &mut send, message_id.as_ref().unwrap(), &events, &mut written_here).await { break false; }
                }
                _ = tick.tick() => {
                    let presence = PeerFrame::Presence {
                        presence: admitted.presence.clone(),
                    };
                    if write_frame(&mut send, &presence, DEADLINE).await.is_err() {
                        tokio::time::timeout(DEADLINE, connection.closed()).await.unwrap();
                        break false;
                    }
                    if message_at.is_some() {
                        heartbeats += 1;
                    }
                }
            }
        };
        diagnostic(format_args!(
            "connection_end stable={} replay={} written={} stopped={}",
            connection.stable_id(),
            replay,
            written_here,
            stopped
        ));
        drop(reading);
        if stopped {
            connection.close(0u32.into(), b"fixture stopped after observation");
            break;
        }
        if let Some(at) = message_at
            && !replay
        {
            // No fixture close occurs before this observation. The daemon
            // must retire the withheld-receipt connection itself.
            observe(
                &events,
                Event::Closed {
                    connection: connection.stable_id(),
                    elapsed: at.elapsed(),
                    heartbeats,
                },
            );
        }
    }
    assert!(
        receipts.written,
        "actual replay acknowledgement never written"
    );
    drop(retained);
    admitted.endpoint.shutdown().await;
}
