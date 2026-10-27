//! Original package-management grammar; resolution and installation are backend-owned.
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginClient {
    Claude,
    Codex,
    Cursor,
    Grok,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginListScope {
    #[default]
    User,
    Project,
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginMutationScope {
    #[default]
    User,
    Project,
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum PluginCommand {
    /// Request installed packages or the selected catalog inventory.
    List {
        #[arg(long)]
        available: bool,
        #[arg(long, value_enum)]
        client: Option<PluginClient>,
        #[arg(long, value_enum, default_value = "user")]
        scope: PluginListScope,
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
    },
    /// Request installation of a package for one client.
    Add {
        #[arg(value_name = "PACKAGE")]
        name: String,
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        #[arg(long, value_enum)]
        client: PluginClient,
        #[arg(long, value_enum, default_value = "user")]
        scope: PluginMutationScope,
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
        /// Accepted without expanding the requested operation's authority.
        #[arg(long)]
        yes: bool,
    },
    /// Request removal of an owned installation for one client.
    Remove {
        #[arg(value_name = "ID")]
        name: String,
        #[arg(long, value_enum)]
        client: PluginClient,
        #[arg(long, value_enum, default_value = "user")]
        scope: PluginMutationScope,
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// Accepted without expanding the requested operation's authority.
        #[arg(long)]
        yes: bool,
    },
}
