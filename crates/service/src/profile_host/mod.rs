//! Explicit registered process integration; replies are evidence, never authority.
mod registration;
pub use crate::native_validation::NativeValidationFailure as Error;
pub use registration::{Registration, RegistrationOptions, StoredProfile};
pub type Result<T> = std::result::Result<T, Error>;

mod blob;
mod client;
mod json;
#[cfg(unix)]
pub(crate) mod process;
pub use blob::Blob;
mod configured;
mod facts;
pub use configured::{Artifacts, Configured, RestorePreparation};
#[cfg(all(test, unix))]
mod client_tests;
mod comparison;
mod projection;
#[cfg(test)]
mod tests;
