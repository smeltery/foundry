use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Debug, Default, Args, Serialize)]
pub struct UpdateOptions {
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum ProviderCommand {
    Connect {
        provider: String,
        #[arg(long, conflicts_with_all = ["browser", "token_stdin"])]
        pair: Option<String>,
        #[arg(long, conflicts_with = "token_stdin")]
        browser: bool,
        #[arg(long)]
        token_stdin: bool,
    },
}
