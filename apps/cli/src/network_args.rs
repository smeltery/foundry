//! Shared network command grammar; parsing does not start network services.
use clap::{Args, Parser, Subcommand};
use serde::{Serialize, Serializer};

#[derive(Debug, Parser)]
#[command(name = "f5y-network", version = env!("FOUNDRY_VERSION"), about = "Network command interface")]
pub struct NetworkCli {
    #[command(subcommand)]
    pub command: Option<StandaloneCommand>,
}

#[derive(Debug, Args, Serialize)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_precedence_over_arg = true,
    version = env!("FOUNDRY_VERSION")
)]
pub struct NetworkOptions {
    #[command(subcommand)]
    pub command: Option<NetworkCommand>,
    #[arg(requires = "message")]
    pub address: Option<String>,
    #[arg(requires = "address", value_parser = crate::args::nonempty)]
    pub message: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum NetworkCommand {
    #[command(flatten)]
    Standalone(StandaloneCommand),
    /// Request an adapter to serve this folder.
    Serve { agent: String },
}
impl Serialize for NetworkCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Standalone(command) => command.serialize(serializer),
            Self::Serve { agent } => {
                serde_json::json!({"operation":"serve","arguments":{"agent":agent}})
                    .serialize(serializer)
            }
        }
    }
}

#[derive(Debug, Subcommand, Serialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case")]
pub enum StandaloneCommand {
    /// Query the agent directory.
    Who {
        filter: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Request message delivery.
    Send {
        to: String,
        message: Vec<String>,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long)]
        fyi: bool,
        #[arg(long, conflicts_with = "fyi")]
        notify: bool,
        #[arg(long)]
        from: Option<String>,
        #[arg(long = "file", value_name = "PATH")]
        files: Vec<String>,
    },
    /// Update an agent's activity.
    Status {
        doing: Vec<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Request a new team.
    Init {
        team: Option<String>,
        #[arg(long = "as")]
        person: Option<String>,
        #[arg(long)]
        no_setup: bool,
        #[arg(long)]
        relay: Option<String>,
        #[arg(long)]
        proof: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Request enrollment information.
    Invite {
        #[arg(long)]
        me: bool,
        #[arg(long)]
        code: bool,
        #[arg(long, default_value_t = 24)]
        hours: u64,
    },
    /// Query or replace the team link.
    Link {
        #[arg(long)]
        new: bool,
        #[arg(long)]
        json: bool,
    },
    /// Request team enrollment.
    Join {
        invite: Option<String>,
        #[arg(long = "as")]
        person: Option<String>,
        #[arg(long)]
        no_setup: bool,
        #[arg(long)]
        proof: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Query team membership.
    Team {
        #[arg(long)]
        json: bool,
    },
    /// Submit an identity assertion.
    Proof { jws: String },
    /// Query the machine endpoint.
    Endpoint,
    /// Request membership removal.
    Remove { who: String },
    /// Request departure from a team.
    Leave {
        #[arg(long)]
        force: bool,
    },
    /// Select a relay configuration.
    Relay { url: String },
    /// Change a folder's network participation.
    Folder {
        path: String,
        #[arg(value_parser = ["off", "on"])]
        state: String,
    },
    /// Query pending inbound messages.
    Held,
    /// Release selected inbound messages.
    Approve {
        ids: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Discard selected inbound messages.
    Reject {
        ids: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Request integration setup changes.
    Setup {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        remove: bool,
        #[arg(long)]
        no_service: bool,
    },
    /// Request network diagnostics.
    Doctor,
    /// Request a network artifact update.
    Upgrade,
    #[command(hide = true)]
    Hook {
        agent: String,
        event: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        doing: Option<String>,
    },
    /// Request foreground service operation.
    Daemon {
        #[arg(long)]
        service: bool,
    },
    /// Request network service shutdown.
    Stop,
    /// Request an inbox observation.
    Listen {
        #[arg(long = "as", default_value = "agent")]
        name: String,
        #[arg(long, default_value_t = 600)]
        wait: u64,
    },
    /// Request the network tool protocol.
    Mcp,
    /// Request the adapter protocol.
    Bridge,
    #[command(hide = true)]
    Tray,
}

impl StandaloneCommand {
    pub fn json(&self) -> bool {
        match self {
            Self::Who { json, .. }
            | Self::Init { json, .. }
            | Self::Join { json, .. }
            | Self::Link { json, .. }
            | Self::Team { json } => *json,
            _ => false,
        }
    }
    pub(crate) fn json_mut(&mut self) -> Option<&mut bool> {
        match self {
            Self::Who { json, .. }
            | Self::Init { json, .. }
            | Self::Join { json, .. }
            | Self::Link { json, .. }
            | Self::Team { json } => Some(json),
            _ => None,
        }
    }
}
