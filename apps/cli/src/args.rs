use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

use crate::session::SessionCommand;
use crate::{account, computer};

#[derive(Debug, Parser)]
#[command(
    name = "f5y",
    version = env!("FOUNDRY_VERSION"),
    about = "Work with Foundry sessions, agents and tools",
    subcommand_negates_reqs = true,
    subcommand_precedence_over_arg = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Local session database; its parent directory must already exist.
    #[arg(long, global = true, value_name = "PATH")]
    pub database: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Task to start; interactive sessions are unsupported by the local backend.
    pub prompt: Option<String>,
    /// Grant one workspace tool operation for this task.
    #[arg(long = "tool-grant", value_enum, requires = "prompt")]
    pub tool_grants: Vec<ToolGrant>,
    #[command(flatten)]
    pub options: PromptOptions,
    #[command(flatten)]
    pub output: OutputOptions,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Default, Args)]
pub struct OutputOptions {
    /// Emit structured JSON.
    #[arg(long, global = true, conflicts_with = "output_format")]
    pub json: bool,
    /// Select the result encoding.
    #[arg(long, global = true, value_enum)]
    pub output_format: Option<OutputFormat>,
}

impl OutputOptions {
    pub fn format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.output_format.unwrap_or_default()
        }
    }
}

/// Independent workspace tool grants; confirmation flags confer no authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
pub enum ToolGrant {
    #[value(name = "read")]
    #[serde(rename = "read")]
    Read,
    #[value(name = "list")]
    #[serde(rename = "list")]
    List,
    #[value(name = "write.create")]
    #[serde(rename = "write.create")]
    WriteCreate,
    #[value(name = "write.overwrite")]
    #[serde(rename = "write.overwrite")]
    WriteOverwrite,
}

/// Optional task effort requested from a capable backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Default, Args, Serialize)]
pub struct PromptOptions {
    #[arg(long, conflicts_with = "on")]
    pub cloud: bool,
    #[arg(long, value_name = "NAME")]
    pub on: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Request detached execution (unsupported by the local backend).
    #[arg(long, short = 'd')]
    pub detach: bool,
    #[arg(long, conflicts_with = "no_repo")]
    pub repo: Option<String>,
    #[arg(long)]
    pub no_repo: bool,
    #[arg(long)]
    pub model: Option<String>,
    /// Requested effort; local execution requires explicit adapter configuration.
    #[arg(long, value_enum)]
    pub effort: Option<Effort>,
    #[arg(long)]
    pub resume: Option<String>,
    #[arg(long)]
    pub update: bool,
    /// Request a standalone CLI update.
    #[arg(long, conflicts_with = "update")]
    pub upgrade: bool,
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum Command {
    /// Serve local session tools over MCP standard input and output.
    Mcp,
    /// Run a task synchronously; the local backend does not support --detach.
    Run(RunOptions),
    /// Inspect and manage sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// List sessions.
    Ls(crate::session::ListOptions),
    /// Follow a session.
    Watch(crate::session::WatchOptions),
    /// Queue an instruction without starting or resuming execution.
    Send {
        id: String,
        #[arg(value_parser = nonempty)]
        message: String,
    },
    /// Stop a session.
    Cancel(crate::session::CancelOptions),
    /// Discover agents and exchange messages.
    Network(NetworkOptions),
    /// Manage Foundry client integrations.
    #[command(subcommand)]
    Plugins(PluginCommand),
    /// Inspect or change Foundry settings.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Connect this computer to a workspace.
    #[command(alias = "device")]
    Connect(computer::ConnectOptions),
    /// Remove this computer's connection or its named workers.
    Disconnect {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Serve tasks from a named worker queue.
    Serve(computer::ServeOptions),
    /// Stop this computer's workers for a queue.
    Stop { name: String },
    /// Inspect or manage worker queues.
    Machines(computer::MachineOptions),
    /// Manage desktop capabilities.
    #[command(subcommand)]
    Desktop(computer::DesktopCommand),
    /// Inspect or request operating-system permissions.
    Permissions(computer::PermissionOptions),
    /// Start account authentication.
    Login,
    /// Disconnect the current account.
    Logout,
    /// Inspect the current account and workspace.
    Whoami,
    /// Check for or install a verified update.
    #[command(alias = "upgrade")]
    Update(account::UpdateOptions),
    /// Connect model-provider accounts.
    #[command(subcommand)]
    Providers(account::ProviderCommand),
    /// Serve the editor agent protocol through standard input and output.
    Acp(PromptOptions),
    /// Inspect the backend and local connection.
    Status,
    /// Print the installed CLI version.
    Version,
    /// Print help for a command, or all commands.
    Help {
        #[arg(value_name = "COMMAND")]
        command: Vec<String>,
    },
}

#[derive(Debug, Args, Serialize)]
pub struct RunOptions {
    #[arg(long, value_parser = nonempty)]
    pub task: String,
    /// Grant one workspace tool operation for this task.
    #[arg(long = "tool-grant", value_enum)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_grants: Vec<ToolGrant>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long = "ref")]
    pub git_ref: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Requested effort; local execution requires explicit adapter configuration.
    #[arg(long, value_enum)]
    pub effort: Option<Effort>,
    /// Request detached execution (unsupported by the local backend).
    #[arg(long, short = 'd')]
    pub detach: bool,
}

pub use crate::network_args::{NetworkCommand, NetworkOptions};

pub use crate::plugin_args::PluginCommand;

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum ConfigCommand {
    List,
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
}

pub fn nonempty(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("value must not be empty".into())
    } else {
        Ok(value.into())
    }
}
