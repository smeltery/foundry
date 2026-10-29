//! Application operations joining durable storage and isolated adapter supervision.
mod admission;
mod context_commit;
mod context_cut;
mod control;
mod coordinated;
mod events;
mod execution;
pub mod fresh_capture_owner;
mod lease;
pub mod native_publication;
pub mod native_validation;
pub mod permission;
mod permission_gate;
pub mod prepared_adapter;
mod publication;
pub mod restoration_owner;
mod run_lease;
mod tool_digest;
mod tool_worker;

use foundry_contracts::{CancelMode, CreateSession, Event, Message, Session};
use foundry_runtime::RuntimeError;
use foundry_store::{Store, StoreError};
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug)]
pub enum ServiceError {
    Store(StoreError),
    Runtime(RuntimeError),
    Invalid(&'static str),
    Coordination(&'static str),
}
impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Runtime(error) => error.fmt(f),
            Self::Invalid(message) => write!(f, "invalid service configuration: {message}"),
            Self::Coordination(message) => write!(f, "execution coordination failed: {message}"),
        }
    }
}
impl std::error::Error for ServiceError {}
impl From<StoreError> for ServiceError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<RuntimeError> for ServiceError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}
pub type Result<T> = std::result::Result<T, ServiceError>;

#[derive(Debug, Clone, Copy)]
pub struct ServiceOptions {
    pub lease_duration: Duration,
    pub poll_interval: Duration,
}
impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            lease_duration: Duration::from_secs(60),
            poll_interval: Duration::from_millis(250),
        }
    }
}

pub struct Service {
    store: Store,
    path: PathBuf,
    options: ServiceOptions,
}
impl Service {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, ServiceOptions::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: ServiceOptions) -> Result<Self> {
        if path.as_ref().as_os_str().is_empty() || path.as_ref() == Path::new(":memory:") {
            return Err(ServiceError::Invalid(
                "a persistent database path is required",
            ));
        }
        if options.poll_interval.is_zero() || options.poll_interval >= options.lease_duration / 3 {
            return Err(ServiceError::Invalid(
                "poll interval must be positive and less than one third of the lease",
            ));
        }
        let path = std::path::absolute(path)
            .map_err(|_| ServiceError::Invalid("cannot resolve database path"))?;
        let store = Store::open_with_lease(&path, options.lease_duration)?;
        Ok(Self {
            store,
            path,
            options,
        })
    }
    pub fn create(&mut self, request: CreateSession) -> Result<Session> {
        Ok(self.store.create(request)?)
    }
    pub fn get(&self, id: &str) -> Result<Session> {
        Ok(self.store.get(id)?)
    }
    pub fn list(&self, limit: u32) -> Result<Vec<Session>> {
        Ok(self.store.list(limit)?)
    }
    pub fn events(&self, id: &str, after: u64, limit: u32) -> Result<Vec<Event>> {
        Ok(self.store.events(id, after, limit)?)
    }
    pub fn cancel(&mut self, id: &str, mode: CancelMode) -> Result<Session> {
        Ok(self.store.request_cancel(id, mode)?)
    }
    pub fn enqueue_message(&mut self, session_id: &str, id: &str, text: &str) -> Result<Message> {
        Ok(self.store.enqueue_message(session_id, id, text)?)
    }
    pub fn messages(&self, session_id: &str, after: u64, limit: u32) -> Result<Vec<Message>> {
        Ok(self.store.messages(session_id, after, limit)?)
    }
    pub fn withdraw_message(&mut self, session_id: &str, id: &str) -> Result<Message> {
        Ok(self.store.withdraw_message(session_id, id)?)
    }
    pub fn recover_expired(&mut self) -> Result<Vec<Session>> {
        Ok(self.store.recover_expired()?)
    }
}
