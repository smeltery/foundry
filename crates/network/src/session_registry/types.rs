use std::{fmt, path::PathBuf};

pub const MAX_TEXT_BYTES: usize = 512;
pub const MAX_SESSIONS: usize = 1024;
pub const MAX_ASSOCIATIONS: usize = 32;
pub const MAX_RESULTS: usize = 128;
pub const MAX_FOLDER_RULES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    InvalidText,
    Limit,
    Unauthorized,
    NotFound,
    Ambiguous,
    Excluded,
    InvalidPath,
    Entropy,
}
impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session registry: {self:?}")
    }
}
impl std::error::Error for RegistryError {}
pub type Result<T> = std::result::Result<T, RegistryError>;

/// Issued by the daemon for a live IPC connection, never accepted from JSON.
/// The daemon must allocate fresh IDs and call disconnect before retiring one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

/// A bearer secret scoped to exactly one conversation. Do not log or serialize.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionCredential(pub(super) [u8; 32]);
impl fmt::Debug for SessionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionCredential(<redacted>)")
    }
}
impl SessionCredential {
    /// The IPC owner may transport this through its private authenticated channel.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub person: String,
    pub machine: String,
    pub name: String,
}

/// Only advertise capabilities backed by an adapter installed by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryCapability {
    Pull,
    Hook,
    Push,
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub conversation: Conversation,
    pub repository: PathBuf,
    pub share_path: bool,
}
#[derive(Debug)]
pub struct Registered {
    pub conversation: Conversation,
    pub credential: SessionCredential,
}
#[derive(Debug, Clone, Default)]
pub struct DirectoryFilter {
    pub person: Option<String>,
    pub machine: Option<String>,
    pub name: Option<String>,
    /// Exact canonical repository, matched only when path sharing is enabled.
    pub repository: Option<PathBuf>,
    pub online_only: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub conversation: Conversation,
    pub repository: Option<PathBuf>,
    pub online: bool,
    pub status: String,
    pub doing: String,
    pub capabilities: Vec<DeliveryCapability>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    pub conversation: Conversation,
    pub associations: Vec<(ConnectionId, DeliveryCapability)>,
}

pub(super) fn text(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.chars().any(char::is_control) {
        return Err(RegistryError::InvalidText);
    }
    Ok(())
}
pub(super) fn identity(value: &Conversation) -> Result<()> {
    for field in [&value.person, &value.machine, &value.name] {
        text(field)?;
    }
    Ok(())
}
