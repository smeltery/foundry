//! Default-deny workspace tools. This library is not a process or OS sandbox.
mod filesystem;
pub mod host;

use cap_std::fs::Dir;
use serde::Deserialize;
use serde_json::Value;
use std::{fmt, io, path::Path};

#[derive(Debug, Clone, Copy, Default)]
pub struct Grants {
    pub read: bool,
    pub list: bool,
    pub create: bool,
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub request_bytes: usize,
    pub read_bytes: usize,
    pub write_bytes: usize,
    pub output_bytes: usize,
    pub entries: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            request_bytes: 2 * 1024 * 1024,
            read_bytes: 256 * 1024,
            write_bytes: 256 * 1024,
            output_bytes: 2 * 1024 * 1024,
            entries: 256,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<()> {
        if self.request_bytes == 0
            || self.request_bytes > 2 * 1024 * 1024
            || self.read_bytes == 0
            || self.read_bytes > 1024 * 1024
            || self.write_bytes == 0
            || self.write_bytes > 1024 * 1024
            || self.output_bytes < 128
            || self.output_bytes > 2 * 1024 * 1024
            || self.entries == 0
            || self.entries > 4096
        {
            return Err(ToolError::Invalid("invalid tool limits"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ToolError {
    UnknownTool,
    Denied,
    Invalid(&'static str),
    Limit(&'static str),
    Unsupported,
    Io(io::ErrorKind),
}
impl ToolError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown_tool",
            Self::Denied => "denied",
            Self::Invalid(_) => "invalid",
            Self::Limit(_) => "limit",
            Self::Unsupported => "unsupported",
            Self::Io(_) => "io",
        }
    }
}
impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool => write!(f, "unknown workspace tool"),
            Self::Denied => write!(f, "workspace tool permission denied"),
            Self::Invalid(message) => write!(f, "invalid tool request: {message}"),
            Self::Limit(message) => write!(f, "tool limit exceeded: {message}"),
            Self::Unsupported => write!(f, "workspace tools are unsupported on this platform"),
            Self::Io(kind) => write!(f, "workspace I/O failure ({kind:?})"),
        }
    }
}
impl std::error::Error for ToolError {}
impl From<io::Error> for ToolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
pub type Result<T> = std::result::Result<T, ToolError>;

pub struct WorkspaceTools {
    root: Dir,
    grants: Grants,
    limits: Limits,
}
impl WorkspaceTools {
    /// The trusted caller selects the root once. Requests never supply ambient paths.
    pub fn open(root: impl AsRef<Path>, grants: Grants, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if !cfg!(unix) {
            return Err(ToolError::Unsupported);
        }
        let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        Self::from_dir(root, grants, limits)
    }

    /// Consume a capability supplied by the trusted caller without reopening its path.
    pub fn from_dir(root: Dir, grants: Grants, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if !cfg!(unix) {
            return Err(ToolError::Unsupported);
        }
        if !root.dir_metadata()?.is_dir() {
            return Err(ToolError::Invalid(
                "workspace capability must be a directory",
            ));
        }
        Ok(Self {
            root,
            grants,
            limits,
        })
    }

    /// Input is bounded before JSON parsing; results fit the configured encoded byte cap.
    pub fn execute_json(&self, name: &str, input: &[u8]) -> Result<Value> {
        if !matches!(name, "read" | "write" | "list") {
            return Err(ToolError::UnknownTool);
        }
        let allowed = match name {
            "read" => self.grants.read,
            "list" => self.grants.list,
            _ => self.grants.create || self.grants.overwrite,
        };
        if !allowed {
            return Err(ToolError::Denied);
        }
        if input.len() > self.limits.request_bytes {
            return Err(ToolError::Limit("request bytes"));
        }
        let result = match name {
            "read" => {
                let args: PathInput = parse(input)?;
                self.read(&args.path)?
            }
            "list" => {
                let args: PathInput = parse(input)?;
                self.list(&args.path)?
            }
            _ => {
                let args: WriteInput = parse(input)?;
                self.write(args)?
            }
        };
        self.check_output(&result)?;
        Ok(result)
    }

    fn check_output(&self, value: &Value) -> Result<()> {
        let encoded =
            serde_json::to_vec(value).map_err(|_| ToolError::Invalid("cannot encode output"))?;
        if encoded.len() > self.limits.output_bytes {
            return Err(ToolError::Limit("output bytes"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
    mode: WriteMode,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WriteMode {
    Create,
    Overwrite,
}
fn parse<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T> {
    serde_json::from_slice(input)
        .map_err(|_| ToolError::Invalid("input does not match the tool schema"))
}
