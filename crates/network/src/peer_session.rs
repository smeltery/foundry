//! Admitted, bounded TEAM sessions. No join/discovery or implicit trust defaults.
//! Dropping a session closes QUIC. Cancelling an individual operation fences the
//! session: drop it and reconnect rather than reuse partially consumed streams.
#[path = "peer_session/admission_fence.rs"]
mod admission_fence;
#[path = "peer_session/authority.rs"]
mod authority;
#[path = "peer_session/driver.rs"]
mod driver;
pub(crate) use admission_fence::AdmissionFence;
#[path = "peer_session/frames.rs"]
mod frames;
#[path = "peer_session/io.rs"]
mod io;
#[path = "peer_session/reader.rs"]
mod reader;
pub use driver::{DriverEvent, PeerDriver, PeerSender};
pub use reader::{PeerReader, ReceivedFrame};
#[path = "peer_session/mail.rs"]
mod mail;
use crate::{Channel, NetworkEndpoint, PeerFrame, WireProfile, wire};
pub use authority::{
    AdmittedTeam, CodePermit, IssuedInvitation, JoinAdmission, PublicBootstrap, PublicCodeBootstrap,
};
use iroh::{
    EndpointAddr, EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
pub use mail::TextMailbox;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::watch;

pub const MAX_PENDING: usize = 64;
pub const MAX_PRESENCE: usize = 128;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerError {
    Invalid,
    Unauthorized,
    DifferentTeam,
    Protocol,
    Storage,
    Quiescent,
    Limit,
    Unsupported,
    UnexpectedAck,
    StaleEpoch,
    Transport,
    TimedOut,
    Closed,
}
impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "peer session: {self:?}")
    }
}
impl std::error::Error for PeerError {}
pub type Result<T> = std::result::Result<T, PeerError>;

/// Bounded warning from an authenticated same-team Hello. It does not claim
/// support for unknown frame variants or change profile/signature validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolMismatch {
    pub expected: u32,
    pub observed: u32,
}

/// Profile must match the endpoint and the store's caller-selected policy.
/// Numeric Hello version mismatch is observed without refusing State exchange.
pub struct SessionOptions {
    pub profile: WireProfile,
    pub deadline: Duration,
    /// true or sender closure cancels I/O. Keep the sender alive while serving.
    pub cancel: watch::Receiver<bool>,
}
/// No Debug: remote presence may contain paths; message content stays in storage.
pub enum PeerEvent {
    StateCommitted {
        revision: u64,
    },
    Presence {
        peer: EndpointId,
        presence: wire::Presence,
    },
    Received {
        message_id: String,
        duplicate: bool,
    },
    /// Durable receiver receipt, NOT adapter injection or local outbox settlement.
    Queued {
        message_id: String,
    },
}

pub struct PeerSession {
    connection: Connection,
    send: SendStream,
    receive: Option<RecvStream>,
    binding: Arc<()>,
    team: Arc<AdmittedTeam>,
    peer: EndpointId,
    options: SessionOptions,
    pending: BTreeMap<String, u64>,
    usable: bool,
    mismatch: Option<ProtocolMismatch>,
}
impl PeerSession {
    /// At most one bounded observation from the authenticated Hello. Hosts should
    /// report it as a warning; it does not assert support for new frame variants.
    pub fn protocol_mismatch(&self) -> Option<ProtocolMismatch> {
        self.mismatch
    }
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    pub async fn connect(
        endpoint: &NetworkEndpoint,
        address: EndpointAddr,
        team: Arc<AdmittedTeam>,
        mut options: SessionOptions,
    ) -> Result<Self> {
        io::options(&options)?;
        if endpoint.id() != team.local() {
            return Err(PeerError::Unauthorized);
        }
        let connection = io::bounded(
            &mut options.cancel,
            options.deadline,
            endpoint.connect(address, Channel::Team, options.deadline),
        )
        .await??;
        Self::establish(connection, team, options, true).await
    }
    pub async fn accept(
        endpoint: &NetworkEndpoint,
        team: Arc<AdmittedTeam>,
        mut options: SessionOptions,
    ) -> Result<Self> {
        io::options(&options)?;
        if endpoint.id() != team.local() {
            return Err(PeerError::Unauthorized);
        }
        let connection = io::bounded(
            &mut options.cancel,
            options.deadline,
            endpoint.accept(options.deadline),
        )
        .await??;
        Self::establish(connection, team, options, false).await
    }
    async fn establish(
        connection: Connection,
        team: Arc<AdmittedTeam>,
        mut options: SessionOptions,
        outgoing: bool,
    ) -> Result<Self> {
        if connection.alpn() != options.profile.team_alpn {
            connection.close(1u32.into(), b"channel refused");
            return Err(PeerError::Protocol);
        }
        let peer = connection.remote_id();
        // Close even if this future is cancelled before a stream/session exists.
        let mut closer = io::CloseGuard(Some(connection.clone()));
        let _permit =
            io::bounded(&mut options.cancel, options.deadline, team.authorize(peer)).await??;
        let streams = if outgoing {
            io::bounded(&mut options.cancel, options.deadline, connection.open_bi()).await?
        } else {
            io::bounded(
                &mut options.cancel,
                options.deadline,
                connection.accept_bi(),
            )
            .await?
        }
        .map_err(|_| PeerError::Transport)?;
        drop(_permit);
        let mut session = Self {
            connection,
            send: streams.0,
            receive: Some(streams.1),
            binding: Arc::new(()),
            team,
            peer,
            options,
            pending: BTreeMap::new(),
            usable: false,
            mismatch: None,
        };
        closer.0 = None;
        session.handshake().await?;
        session.usable = true;
        Ok(session)
    }
    async fn handshake(&mut self) -> Result<()> {
        let permit = self.permit().await?;
        let hello = PeerFrame::Hello {
            team: permit.document.id.clone(),
            proto: self.options.profile.protocol,
        };
        let remote = io::exchange(
            &mut self.send,
            self.receive.as_mut().ok_or(PeerError::Closed)?,
            &hello,
            &mut self.options,
        )
        .await?;
        match remote {
            PeerFrame::Hello { team, proto } => {
                if team != permit.document.id {
                    return Err(PeerError::DifferentTeam);
                }
                if proto != self.options.profile.protocol {
                    self.mismatch = Some(ProtocolMismatch {
                        expected: self.options.profile.protocol,
                        observed: proto,
                    });
                }
            }
            _ => return Err(PeerError::Protocol),
        }
        drop(permit);
        let permit = self.permit().await?;
        let state = frames::encode_state(&permit.document)?;
        let remote = io::exchange(
            &mut self.send,
            self.receive.as_mut().ok_or(PeerError::Closed)?,
            &state,
            &mut self.options,
        )
        .await?;
        drop(permit);
        let PeerFrame::State { state } = remote else {
            return Err(PeerError::Protocol);
        };
        self.merge_state(frames::decode_state(*state)?).await?;
        Ok(())
    }
    fn begin(&mut self) -> Result<()> {
        if !self.usable {
            return Err(PeerError::Closed);
        }
        self.usable = false;
        Ok(())
    }
    fn completed<T>(&mut self, result: Result<T>) -> Result<T> {
        self.usable = result.is_ok();
        if result.is_err() {
            self.connection.close(1u32.into(), b"session refused");
        }
        result
    }
    pub async fn publish_state(&mut self) -> Result<()> {
        self.begin()?;
        let result = async {
            let permit = self.permit().await?;
            io::write(
                &mut self.send,
                &frames::encode_state(&permit.document)?,
                &mut self.options,
            )
            .await
        }
        .await;
        self.completed(result)
    }
    /// Advertise only capabilities actually supplied by the caller's adapters.
    pub async fn publish_presence(&mut self, mut presence: wire::Presence) -> Result<()> {
        self.begin()?;
        let result = async {
            let permit = self.permit().await?;
            frames::presence(&mut presence, &permit.local)?;
            io::write(
                &mut self.send,
                &PeerFrame::Presence { presence },
                &mut self.options,
            )
            .await
        }
        .await;
        self.completed(result)
    }
    /// Caller persists its outbound wire message before calling this; the pending
    /// correlation here is volatile and never marks an outbox delivered. Reconnect
    /// may resend the same immutable message after a lost receipt ACK.
    pub async fn send_text(&mut self, message: wire::Message) -> Result<()> {
        self.begin()?;
        let result = async {
            if self.pending.len() >= MAX_PENDING || self.pending.contains_key(&message.id) {
                return Err(PeerError::Limit);
            }
            let permit = self.permit().await?;
            frames::message(&message, &permit.local, &permit.remote)?;
            io::write(
                &mut self.send,
                &PeerFrame::Msg {
                    msg: Box::new(message.clone()),
                },
                &mut self.options,
            )
            .await?;
            self.pending.insert(message.id, permit.revision);
            Ok(())
        }
        .await;
        self.completed(result)
    }
    pub async fn receive(&mut self) -> Result<PeerEvent> {
        self.begin()?;
        let result = async {
            let frame = io::read(
                self.receive.as_mut().ok_or(PeerError::Closed)?,
                &mut self.options,
            )
            .await?;
            self.handle_frame(frame).await
        }
        .await;
        self.completed(result)
    }
}
impl Drop for PeerSession {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"session closed");
    }
}
impl From<crate::NetworkError> for PeerError {
    fn from(error: crate::NetworkError) -> Self {
        match error {
            crate::NetworkError::TimedOut => Self::TimedOut,
            crate::NetworkError::Closed => Self::Closed,
            crate::NetworkError::Invalid(_) | crate::NetworkError::Json(_) => Self::Invalid,
            _ => Self::Transport,
        }
    }
}
