use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

use crate::args::{Effort, ToolGrant, nonempty};

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum SessionCommand {
    List(ListOptions),
    /// Create a session on the configured backend.
    Start {
        #[arg(long, value_parser = nonempty)]
        prompt: String,
        /// Grant one workspace tool operation for this task.
        #[arg(long = "tool-grant", value_enum)]
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_grants: Vec<ToolGrant>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, value_enum)]
        provider: Option<Provider>,
        #[arg(long)]
        model: Option<String>,
        /// Requested effort; local execution requires explicit adapter configuration.
        #[arg(long, value_enum)]
        effort: Option<Effort>,
    },
    Get {
        id: String,
    },
    Watch(WatchOptions),
    Send {
        id: String,
        #[arg(long, value_parser = nonempty)]
        message: String,
    },
    Cancel(CancelOptions),
    Attachments {
        id: String,
    },
    Download {
        id: String,
        attachment: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Register a private HTTPS link supplied through standard input.
    ShareLink {
        id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expires_in: Option<u64>,
    },
}

#[derive(Debug, Args, Serialize)]
pub struct ListOptions {
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
    #[arg(long, value_enum)]
    pub status: Option<ArchiveFilter>,
    #[arg(long, value_parser = ["ui", "external", "external:cli", "external:mcp", "external:api"])]
    pub origin: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub fields: Vec<String>,
}

#[derive(Debug, Args, Serialize)]
pub struct WatchOptions {
    pub id: String,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: Option<u64>,
}

#[derive(Debug, Args, Serialize)]
pub struct CancelOptions {
    pub id: String,
    #[arg(long)]
    pub hard: bool,
}

#[derive(Debug, Clone, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFilter {
    Active,
    Archived,
    All,
}

#[derive(Debug, Clone, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Github,
    Gitlab,
}
