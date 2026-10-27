use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Args, Serialize)]
pub struct ConnectOptions {
    /// Deprecated; accepted but ignored.
    #[arg(long, value_name = "N")]
    #[serde(skip_serializing)]
    pub sessions: Option<String>,
    /// Deprecated; accepted but ignored.
    #[arg(long, value_enum)]
    #[serde(skip_serializing)]
    pub cloud_sessions: Option<Toggle>,
    #[arg(long, conflicts_with = "token")]
    pub to: Option<String>,
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long)]
    pub api_key: Option<String>,
    #[arg(long)]
    pub api_url: Option<String>,
    #[arg(long)]
    pub once: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub folder: Option<PathBuf>,
    #[arg(long)]
    pub add_folder: Vec<PathBuf>,
    #[arg(long)]
    pub remove_folder: Vec<PathBuf>,
    #[arg(long)]
    pub proxy: Option<String>,
    #[arg(long)]
    pub proxy_ca_file: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub user_chrome: Option<Toggle>,
    #[arg(long, value_enum)]
    pub local_logins: Option<Toggle>,
    #[arg(long, value_enum)]
    pub keep_awake: Option<Toggle>,
    #[arg(long)]
    pub minimal: bool,
}

#[derive(Debug, Args, Serialize)]
pub struct ServeOptions {
    #[arg(conflicts_with = "token")]
    pub name: Option<String>,
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long)]
    pub dir: Option<PathBuf>,
    #[arg(long, conflicts_with = "service")]
    pub once: bool,
    #[arg(long)]
    pub service: bool,
    #[arg(long)]
    pub sandbox: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub stop: bool,
}

#[derive(Debug, Args, Serialize)]
pub struct MachineOptions {
    #[command(subcommand)]
    pub command: Option<MachineCommand>,
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum MachineCommand {
    Add {
        name: String,
    },
    Token {
        name: String,
    },
    Rm {
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum DesktopCommand {
    Install {
        #[arg(long, value_enum, default_value = "none")]
        permissions: PermissionLevel,
    },
    Minimal,
    Status,
    BrowserUse,
    Mentions,
}

#[derive(Debug, Args, Serialize)]
pub struct PermissionOptions {
    #[command(subcommand)]
    pub command: Option<PermissionCommand>,
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum PermissionCommand {
    Allow {
        #[arg(value_enum)]
        permission: Option<PermissionKind>,
    },
}

#[derive(Debug, Clone, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Screen,
    Accessibility,
    Disk,
}

#[derive(Debug, Clone, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    None,
    Standard,
    All,
}

#[derive(Debug, Clone, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    On,
    Off,
}
