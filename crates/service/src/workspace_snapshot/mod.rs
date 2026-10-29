//! Bounded filesystem artifact attempt after owner-verified process cleanup.
//! No native context, durable head selection, or restoration authority is created.
pub use foundry_tools::workspace_snapshot::Limits;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod filesystem;
#[cfg(unix)]
mod supervisor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFailure {
    Invalid,
    Io,
    Limit,
    Changed,
    Unsupported,
    AbortedHard,
    Cleanup,
}
/// An immutable private artifact, not a globally atomic or restored workspace.
/// The caller must durably associate it with the owning attempt before reuse.
pub struct SnapshotArtifact {
    pub directory: PathBuf,
    pub bytes: u64,
    pub entries: u64,
    pub manifest_digest: [u8; 32],
}

/// The trusted owner supplies its fixed absolute toolhost and a private existing
/// destination outside the workspace. Only hard cancellation aborts this attempt.
/// Caller retains the lease and fence, and establishes stopped-owner cleanup.
pub fn attempt(
    helper: &Path,
    workspace: &Path,
    destination: &Path,
    limits: Limits,
    hard_cancelled: impl Fn() -> bool,
) -> Result<SnapshotArtifact, SnapshotFailure> {
    #[cfg(unix)]
    {
        supervisor::attempt(helper, workspace, destination, limits, hard_cancelled)
    }
    #[cfg(not(unix))]
    {
        let _ = (helper, workspace, destination, limits, hard_cancelled);
        Err(SnapshotFailure::Unsupported)
    }
}
impl From<std::io::Error> for SnapshotFailure {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

#[cfg(all(test, unix))]
mod tests;
