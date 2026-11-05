//! Opt-in account rollover. When an attempt ends on a provider account limit,
//! [`decide`] says whether the session continues with the same model on
//! another connected account of the same provider, through a new attempt.
//!
//! The decision is a value, like catalog admission: it reads no store, makes
//! no network call and is not yet wired into the runtime. Rollover is off by
//! default, never happens while an attempt runs, never crosses a workspace or
//! provider, and never targets an account that is unavailable or whose
//! credential needs attention. Every rollover yields a [`RolloverRecord`]; the
//! session's records are also the input that keeps a chain from revisiting an
//! account, so a chain makes at most one rollover per connected account.
//!
//! Known limitations:
//! - Hosted fallback after the last account waits on D46-5, and rollover across
//!   subscription accounts on D46-2.
//! - Wire names and cause codes are provisional pending NIC-34 D1/D6.

mod account;
mod attempt;
mod decision;
mod error;

pub use account::{AccountCandidate, SkipCause, SkippedAccount};
pub use attempt::{AttemptFailure, AttemptId, AttemptState};
pub use decision::{
    HoldCause, RolloverDecision, RolloverPolicy, RolloverRecord, RolloverRequest, RolloverTrigger,
    decide,
};
pub use error::RolloverError;
