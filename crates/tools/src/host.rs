//! Trusted parent IPC only. Never forward adapter-provided authority into this schema.
use crate::{Grants, Limits, ToolError, WorkspaceTools};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};

pub const VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;
pub const MAX_REPLY_BYTES: usize = 1024 * 1024;
const CONTENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Grant {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "list")]
    List,
    #[serde(rename = "write.create")]
    Create,
    #[serde(rename = "write.overwrite")]
    Overwrite,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostLimits {
    pub request_bytes: usize,
    pub read_bytes: usize,
    pub write_bytes: usize,
    pub output_bytes: usize,
    pub entries: usize,
}
impl HostLimits {
    fn checked(&self) -> Result<Limits, HostError> {
        if self.request_bytes > MAX_REQUEST_BYTES
            || self.read_bytes > CONTENT_BYTES
            || self.write_bytes > CONTENT_BYTES
            || self.output_bytes > MAX_REPLY_BYTES
            || self.entries > 256
        {
            return Err(HostError::Policy);
        }
        let limits = Limits {
            request_bytes: self.request_bytes,
            read_bytes: self.read_bytes,
            write_bytes: self.write_bytes,
            output_bytes: self.output_bytes,
            entries: self.entries,
        };
        limits.validate().map_err(|_| HostError::Policy)?;
        Ok(limits)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathArgs {
    pub path: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
    pub mode: WriteMode,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    Create,
    Overwrite,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Input {
    Path(PathArgs),
    Write(WriteArgs),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub attempt_id: String,
    pub call_id: String,
    #[serde(default)]
    pub grants: Vec<Grant>,
    pub limits: HostLimits,
    pub tool: String,
    pub input: Input,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub version: u32,
    pub attempt_id: String,
    pub call_id: String,
    pub result: Outcome,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { output: Value },
    Error { code: ErrorCode, effect: Effect },
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    None,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownTool,
    Denied,
    Invalid,
    Limit,
    Unsupported,
    Io,
}
impl From<&ToolError> for ErrorCode {
    fn from(error: &ToolError) -> Self {
        match error {
            ToolError::UnknownTool => Self::UnknownTool,
            ToolError::Denied => Self::Denied,
            ToolError::Invalid(_) => Self::Invalid,
            ToolError::Limit(_) => Self::Limit,
            ToolError::Unsupported => Self::Unsupported,
            ToolError::Io(_) => Self::Io,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HostError {
    Bootstrap,
    Protocol,
    Policy,
    InputIo,
    OutputIo,
    Unsupported,
}
impl HostError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Bootstrap => "toolhost_bootstrap",
            Self::Protocol => "toolhost_protocol",
            Self::Policy => "toolhost_policy",
            Self::InputIo => "toolhost_input_io",
            Self::OutputIo => "toolhost_output_io",
            Self::Unsupported => "toolhost_unsupported",
        }
    }
}

/// Consume the complete bounded frame AND EOF before any filesystem operation.
/// Parent owns deadlines/cancellation; a pipe deliberately left open will block.
pub fn read_request(reader: &mut impl Read) -> Result<Request, HostError> {
    let mut header = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| HostError::InputIo)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_REQUEST_BYTES {
        return Err(HostError::Protocol);
    }
    let mut bytes = vec![0; size];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| HostError::InputIo)?;
    let mut extra = [0];
    if reader.read(&mut extra).map_err(|_| HostError::InputIo)? != 0 {
        return Err(HostError::Protocol);
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| HostError::Protocol)?;
    if request.version != VERSION
        || !identifier(&request.attempt_id)
        || !identifier(&request.call_id)
        || request.tool.is_empty()
        || request.tool.len() > 64
    {
        return Err(HostError::Protocol);
    }
    if bytes.len() > request.limits.request_bytes {
        return Err(HostError::Policy);
    }
    request.limits.checked()?;
    for (index, grant) in request.grants.iter().enumerate() {
        if request.grants[..index].contains(grant) {
            return Err(HostError::Policy);
        }
    }
    Ok(request)
}

fn identifier(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
}

/// Runs one call. The directory and this request must come from the trusted parent.
pub fn serve(root: Dir, reader: &mut impl Read, writer: &mut impl Write) -> Result<(), HostError> {
    let request = read_request(reader)?;
    let grants = Grants {
        read: request.grants.contains(&Grant::Read),
        list: request.grants.contains(&Grant::List),
        create: request.grants.contains(&Grant::Create),
        overwrite: request.grants.contains(&Grant::Overwrite),
    };
    let tools = WorkspaceTools::from_dir(root, grants, request.limits.checked()?)
        .map_err(|_| HostError::Bootstrap)?;
    let input = serde_json::to_vec(&request.input).map_err(|_| HostError::Protocol)?;
    let result = tools.execute_json(&request.tool, &input);
    let effect = if request.tool == "write" {
        Effect::Unknown
    } else {
        Effect::None
    };
    let outcome = match result {
        Ok(output) => Outcome::Ok { output },
        Err(error) => Outcome::Error {
            code: ErrorCode::from(&error),
            effect: if matches!(error, ToolError::Denied | ToolError::UnknownTool) {
                Effect::None
            } else {
                effect
            },
        },
    };
    let mut reply = Reply {
        version: VERSION,
        attempt_id: request.attempt_id,
        call_id: request.call_id,
        result: outcome,
    };
    let mut encoded = serde_json::to_vec(&reply).map_err(|_| HostError::OutputIo)?;
    if encoded.len() > MAX_REPLY_BYTES {
        reply.result = Outcome::Error {
            code: ErrorCode::Limit,
            effect,
        };
        encoded = serde_json::to_vec(&reply).map_err(|_| HostError::OutputIo)?;
    }
    writer
        .write_all(&(encoded.len() as u32).to_be_bytes())
        .map_err(|_| HostError::OutputIo)?;
    writer
        .write_all(&encoded)
        .map_err(|_| HostError::OutputIo)?;
    writer.flush().map_err(|_| HostError::OutputIo)
}

/// Adopt reserved fd 3 once, after checking it, with no ambient-path fallback.
///
/// # Safety
/// Caller must exclusively own inherited fd 3 and must call this at most once.
/// No other Rust object may own it or concurrently close/reassign it.
#[cfg(unix)]
pub unsafe fn inherited_directory() -> Result<Dir, HostError> {
    use std::os::fd::FromRawFd;
    // fcntl validates the descriptor before constructing its sole Rust owner.
    let flags = unsafe { libc::fcntl(3, libc::F_GETFD) };
    if flags < 0 {
        return Err(HostError::Bootstrap);
    }
    let file = unsafe { std::fs::File::from_raw_fd(3) };
    if unsafe { libc::fcntl(3, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
        || !file.metadata().map_err(|_| HostError::Bootstrap)?.is_dir()
    {
        return Err(HostError::Bootstrap);
    }
    Ok(Dir::from_std_file(file))
}
