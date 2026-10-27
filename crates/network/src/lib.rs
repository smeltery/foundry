//! Authenticated QUIC transport and bounded frames, without team authorization.
//! Profiles and relay choices are explicit. No public discovery is installed.
pub mod full_team_store;
pub mod identity;
pub use identity::{IdentityError, LocalIdentity, load_or_create_identity};

pub mod code_invite;
pub mod codec;
pub mod endpoint;
pub mod membership;
pub mod persistence;
pub mod profile;
pub mod proof;
pub mod signing;
pub mod team;
pub mod team_state;
pub mod ticket;
pub mod wire;

pub use codec::{MAX_FRAME_BYTES, read_frame, write_frame};
pub use endpoint::{EndpointConfig, NetworkEndpoint, RelaySelection};
pub use profile::{Channel, WireProfile};
pub use wire::PeerFrame;

use std::{fmt, io};

#[derive(Debug)]
pub enum NetworkError {
    Invalid(&'static str),
    Io(io::Error),
    Json(serde_json::Error),
    Transport(String),
    TimedOut,
    Closed,
}
impl fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid network input: {message}"),
            Self::Io(error) => write!(formatter, "network I/O: {error}"),
            Self::Json(error) => write!(formatter, "network JSON: {error}"),
            Self::Transport(message) => write!(formatter, "network transport: {message}"),
            Self::TimedOut => write!(formatter, "network operation timed out"),
            Self::Closed => write!(formatter, "network endpoint closed"),
        }
    }
}
impl std::error::Error for NetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for NetworkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for NetworkError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
pub type Result<T> = std::result::Result<T, NetworkError>;
