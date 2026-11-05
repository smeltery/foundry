//! Client for local and custom model endpoints. An endpoint is a provider of
//! kind `local_endpoint` with a base URL, a wire format from its catalog
//! descriptor (`chat` by default) and an explicit list of model IDs.
//!
//! Loopback, LAN and tailnet addresses need an explicit opt-in, public
//! addresses need HTTPS, and link-local or reserved addresses are always
//! refused. Errors carry a neutral class and category only. Model weights are
//! never downloaded and models are never discovered.

mod client;
mod config;
mod error;
mod format;
mod network;

pub use client::LocalEndpoint;
pub use config::{DEFAULT_TIMEOUT, EndpointKey, EndpointSettings, MAX_TIMEOUT};
pub use error::{EndpointError, FailureCategory, InvalidField};
pub use format::{Completion, CompletionRequest, Message, Role};
pub use network::{Admission, NetworkClass, OptIn, Resolve, SystemResolver, classify};
