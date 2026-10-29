//! Private stopped-owner filesystem copy. This is not an atomic filesystem cut.
use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod copy;
#[cfg(unix)]
pub use copy::{open_absolute, sync_directory};

pub const MAX_REQUEST_BYTES: usize = 1024;
pub const MAX_REPLY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub bytes: u64,
    pub entries: u64,
    pub depth: u32,
    pub timeout_ms: u64,
}
impl Limits {
    pub fn valid(self) -> bool {
        self.bytes > 0
            && self.bytes <= 64 * 1024 * 1024 * 1024
            && self.entries > 0
            && self.entries <= 1_000_000
            && self.depth > 0
            && self.depth <= 128
            && self.timeout_ms > 0
            && self.timeout_ms <= 300_000
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub limits: Limits,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Invalid,
    Io,
    Limit,
    Changed,
    Unsupported,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Copied {
        bytes: u64,
        entries: u64,
        manifest_digest: [u8; 32],
    },
    Failed {
        code: Failure,
    },
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

/// Dedicated process entry point: descriptors 3 and 4 are exclusively supplied
/// by the trusted supervisor before any descriptor-owning operation here.
///
/// # Safety
/// Caller exclusively transfers ownership of descriptors 3 and 4, exactly once.
pub unsafe fn serve_inherited() -> Result<(), crate::host::HostError> {
    #[cfg(unix)]
    {
        use std::{
            io::{Read, Write},
            os::fd::FromRawFd,
        };
        let adopt = |fd| -> Result<cap_std::fs::Dir, crate::host::HostError> {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                return Err(crate::host::HostError::Bootstrap);
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
                || !file
                    .metadata()
                    .map_err(|_| crate::host::HostError::Bootstrap)?
                    .is_dir()
            {
                return Err(crate::host::HostError::Bootstrap);
            }
            Ok(cap_std::fs::Dir::from_std_file(file))
        };
        let root = adopt(3)?;
        let stage = adopt(4)?;
        let mut input = Vec::new();
        std::io::stdin()
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_to_end(&mut input)
            .map_err(|_| crate::host::HostError::InputIo)?;
        if input.len() > MAX_REQUEST_BYTES {
            return Err(crate::host::HostError::Protocol);
        }
        let request: Request =
            serde_json::from_slice(&input).map_err(|_| crate::host::HostError::Protocol)?;
        if request.version != 1 || !request.limits.valid() {
            return Err(crate::host::HostError::Policy);
        }
        let reply = match copy::copy(&root, &stage, request.limits) {
            Ok(reply) => reply,
            Err(code) => Reply::Failed { code },
        };
        let bytes = serde_json::to_vec(&reply).map_err(|_| crate::host::HostError::OutputIo)?;
        std::io::stdout()
            .write_all(&bytes)
            .map_err(|_| crate::host::HostError::OutputIo)
    }
    #[cfg(not(unix))]
    {
        Err(crate::host::HostError::Unsupported)
    }
}
