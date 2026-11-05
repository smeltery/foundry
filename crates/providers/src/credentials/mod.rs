//! Provider credential lifecycle: configure, validate, list, check and delete
//! API-key account slots, several per provider, each bound to one workspace.
//!
//! Secrets are write-only. No read, list, error, `Debug` output or check
//! result contains a key; the only way out is [`CredentialHandoff`], which
//! feeds the adapter's private inputs. Validation and account checks go
//! through an injected [`CredentialValidator`]; [`FixtureValidator`] is
//! deterministic and offline, and no live provider validator exists yet.
//!
//! Known limitations:
//! - Secrets are stored unencrypted in an owner-only file; there is no
//!   at-rest key management.
//! - Only API keys are stored. Subscription and OAuth credentials, usage
//!   reads and transport wiring are not implemented.
//! - Wire names, error classes and cause codes are provisional.

mod check;
mod database;
mod error;
mod rows;
mod secret;
mod slot;
mod store;

pub use check::{
    AccountCheck, AuthenticationStatus, CapacityStatus, CheckCause, CredentialValidator, Evidence,
    EvidenceBasis, FixtureValidator, ProviderEvidence, ValidationOutcome, validate,
};
pub use error::CredentialError;
#[cfg(unix)]
pub use secret::EchoGuard;
pub use secret::{MAX_SECRET_BYTES, Secret, read_token, read_token_stdin};
pub use slot::{AccountKey, Caller, CredentialEvent, CredentialSlot, CredentialState, Failure};
pub use store::{
    Clock, ConfigureRequest, Configured, CredentialHandoff, CredentialList, CredentialStore,
    Deleted, MAX_CONFIG_VALUE_BYTES, Readiness,
};
