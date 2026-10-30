//! Bounded text transfer with explicit sender roots and private receiver storage.
//! Callers authenticate peer identity and authorize message/recipient ownership.
use crate::wire::TextAttachment;
use std::{
    fmt,
    path::{Path, PathBuf},
};
#[cfg(unix)]
mod database;
#[cfg(unix)]
mod filesystem;
mod frame_names;
#[cfg(unix)]
mod materialize;
mod names;
#[cfg(unix)]
mod sender;
#[cfg(unix)]
mod storage;
mod validation;
pub use names::safe_name;
#[cfg(unix)]
mod expiration;
#[cfg(unix)]
pub use storage::AttachmentStore;

pub const MAX_FILES: usize = 10;
pub const MAX_FILE_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_BYTES: usize = 512 * 1024;
pub const MAX_KEPT_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_SENDER_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_SENDER_HOURLY_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_BATCHES: u64 = 4096;
pub const RETAIN_RELEASED_MS: i64 = 86_400_000;
pub const RETAIN_UNREAD_MS: i64 = 7 * RETAIN_RELEASED_MS;
pub type Result<T> = std::result::Result<T, AttachmentError>;
/// Static diagnostics do not contain local paths, text, or database parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentError {
    Invalid,
    UnsafePath,
    InvalidText,
    Limit,
    Conflict,
    Quota,
    Missing,
    InUse,
    Retention,
    Deleting,
    Expired,
    Corrupt,
    Storage,
    Unsupported,
}
impl fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid attachment input",
            Self::UnsafePath => "unsafe attachment path",
            Self::InvalidText => "attachment must be UTF-8 text without binary controls",
            Self::Limit => "attachment size or count limit exceeded",
            Self::Conflict => "attachment retry conflicts",
            Self::Quota => "attachment quota reached",
            Self::Missing => "attachments missing",
            Self::InUse => "attachment references have not been released",
            Self::Retention => "attachment retention has not elapsed",
            Self::Deleting => "attachment deletion must finish",
            Self::Expired => "attachment reference expired",
            Self::Corrupt => "invalid attachment storage",
            Self::Storage => "attachment storage failed",
            Self::Unsupported => "attachment storage unsupported",
        })
    }
}
impl std::error::Error for AttachmentError {}
impl From<std::io::Error> for AttachmentError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<rusqlite::Error> for AttachmentError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl From<crate::persistence::PersistenceError> for AttachmentError {
    fn from(_: crate::persistence::PersistenceError) -> Self {
        Self::UnsafePath
    }
}

/// Caller-selected paths define read authority; no process profile is discovered.
/// Path confinement does not establish inode origin or classify content secrets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SenderMode {
    Agent,
    Human,
}
pub struct SenderPolicy {
    workspace: PathBuf,
    allowed: Vec<PathBuf>,
    denied: Vec<PathBuf>,
    mode: SenderMode,
    frames: Vec<String>,
    folders_on: Vec<PathBuf>,
    folders_off: Vec<PathBuf>,
}
impl SenderPolicy {
    /// Explicit folder switches apply in either caller mode. They cannot override agent secret roots.
    pub fn with_folders(mut self, enabled: &[PathBuf], disabled: &[PathBuf]) -> Result<Self> {
        let resolve = |values: &[PathBuf]| -> Result<Vec<PathBuf>> {
            values
                .iter()
                .map(|p| {
                    if p.is_absolute() {
                        canonical_rule(p)
                    } else {
                        Err(AttachmentError::Invalid)
                    }
                })
                .collect()
        };
        self.folders_on = resolve(enabled)?;
        self.folders_off = resolve(disabled)?;
        Ok(self)
    }
    /// Optional explicit personal-root boundaries reject a workspace that contains them.
    /// This reads only supplied paths, not a personal profile or environment variable.
    pub fn agent(
        workspace: &Path,
        allowed: &[PathBuf],
        denied: &[PathBuf],
        personal_roots: &[PathBuf],
    ) -> Result<Self> {
        let policy = Self::new(workspace, allowed, denied)?;
        for root in personal_roots {
            if !root.is_absolute() || root.canonicalize()?.starts_with(&policy.workspace) {
                return Err(AttachmentError::UnsafePath);
            }
        }
        Ok(policy)
    }
    /// Explicit frame vocabulary for the configured renderer, never discovered or embedded.
    pub fn with_frame_words(mut self, words: &[String]) -> Result<Self> {
        frame_names::validate(words)?;
        self.frames = words.to_vec();
        Ok(self)
    }
    /// Human-selected paths have text/size checks but no agent root or secret restrictions.
    /// Relative paths use only the explicit working directory; no profile lookup occurs.
    pub fn human(cwd: &Path) -> Result<Self> {
        if !cwd.is_absolute() {
            return Err(AttachmentError::Invalid);
        }
        let workspace = cwd.canonicalize()?;
        if !workspace.is_dir() {
            return Err(AttachmentError::Invalid);
        }
        Ok(Self {
            workspace,
            allowed: Vec::new(),
            denied: Vec::new(),
            mode: SenderMode::Human,
            frames: Vec::new(),
            folders_on: Vec::new(),
            folders_off: Vec::new(),
        })
    }
    pub fn new(
        workspace: &Path,
        allowed_roots: &[PathBuf],
        denied_roots: &[PathBuf],
    ) -> Result<Self> {
        if !workspace.is_absolute() || allowed_roots.is_empty() {
            return Err(AttachmentError::Invalid);
        }
        let workspace = workspace.canonicalize()?;
        if workspace.parent().is_none() || !workspace.is_dir() {
            return Err(AttachmentError::UnsafePath);
        }
        let resolve = |paths: &[PathBuf]| -> Result<Vec<PathBuf>> {
            paths
                .iter()
                .map(|p| {
                    if !p.is_absolute() {
                        return Err(AttachmentError::Invalid);
                    }
                    canonical_rule(p)
                })
                .collect()
        };
        let allowed = resolve(allowed_roots)?;
        if allowed
            .iter()
            .any(|p| !p.starts_with(&workspace) || !p.is_dir())
        {
            return Err(AttachmentError::UnsafePath);
        }
        Ok(Self {
            workspace,
            allowed,
            denied: resolve(denied_roots)?,
            mode: SenderMode::Agent,
            frames: Vec::new(),
            folders_on: Vec::new(),
            folders_off: Vec::new(),
        })
    }
}
// Resolve only supplied path ancestors; an absent disabled directory still defines a rule.
fn canonical_rule(path: &Path) -> Result<PathBuf> {
    let mut probe = path;
    let mut tail = Vec::new();
    loop {
        match probe.canonicalize() {
            Ok(mut root) => {
                for piece in tail.into_iter().rev() {
                    root.push(piece);
                }
                return Ok(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    probe
                        .file_name()
                        .ok_or(AttachmentError::UnsafePath)?
                        .to_os_string(),
                );
                probe = probe.parent().ok_or(AttachmentError::UnsafePath)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
/// Reads at most limit+1 bytes per regular file. No returned local source paths.
pub fn prepare(policy: &SenderPolicy, paths: &[PathBuf]) -> Result<Vec<TextAttachment>> {
    #[cfg(unix)]
    {
        sender::prepare(policy, paths)
    }
    #[cfg(not(unix))]
    {
        let _ = (policy, paths);
        Err(AttachmentError::Unsupported)
    }
}
/// Text/count/byte checks only; normalize_received supplies safe names and strips peer paths.
/// Never join raw peer names or path fields after calling only this size validator.
pub fn validate_received(files: &[TextAttachment]) -> Result<usize> {
    validation::batch(files)
}
/// Receiver normalization strips peer-supplied paths and independently bounds all content.
pub fn normalize_received(files: &[TextAttachment]) -> Result<Vec<TextAttachment>> {
    names::normalize(files)
}
pub fn normalize_received_with_frames(
    files: &[TextAttachment],
    words: &[String],
) -> Result<Vec<TextAttachment>> {
    frame_names::validate(words)?;
    names::normalize_with(files, words)
}

/// These identifiers come from authenticated routing, not attachment filenames.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentKey {
    pub sender: String,
    pub recipient: String,
    pub message: String,
}
impl AttachmentKey {
    pub fn new(sender: &str, recipient: &str, message: &str) -> Result<Self> {
        for value in [sender, recipient, message] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(AttachmentError::Invalid);
            }
        }
        Ok(Self {
            sender: sender.into(),
            recipient: recipient.into(),
            message: message.into(),
        })
    }
    fn directory(&self) -> Result<String> {
        Self::new(&self.sender, &self.recipient, &self.message)?;
        let mut hash = blake3::Hasher::new_derive_key("foundry attachment ownership v1");
        for value in [&self.sender, &self.recipient, &self.message] {
            hash.update(&(value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        Ok(hash.finalize().to_hex().to_string())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub batches: u64,
    pub bytes: u64,
}

/// The host expires these references under its message ownership gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionReference {
    Unread,
    Held,
    Read { at_ms: i64 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredReference {
    ReadAttachments,
    UnreadAttachments,
    HeldMessage,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryOutcome {
    NotDue {
        expires_ms: i64,
    },
    Expired {
        reference: ExpiredReference,
        cleanup_error: Option<AttachmentError>,
    },
}
