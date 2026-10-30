#![cfg(unix)]
#[path = "daemon_network_process/child.rs"]
mod child;
#[path = "daemon_network_process/client.rs"]
mod client;
#[path = "daemon_network_process/config.rs"]
mod config;
#[path = "daemon_network_process/direct.rs"]
mod direct;
#[path = "daemon_network_process/flow.rs"]
mod flow;
#[path = "daemon_network_process/lifecycle.rs"]
mod lifecycle;
#[path = "daemon_network_process/negative.rs"]
mod negative;
#[path = "daemon_network_process/relay.rs"]
mod relay;

#[path = "daemon_network_process/cancellation.rs"]
mod cancellation;
#[path = "daemon_network_process/clock.rs"]
mod clock;
#[path = "daemon_network_process/database.rs"]
mod database;
#[path = "daemon_network_process/expiry.rs"]
mod expiry;
#[path = "daemon_network_process/receipt.rs"]
mod receipt;
use std::time::Duration;
const DEADLINE: Duration = Duration::from_secs(45);
