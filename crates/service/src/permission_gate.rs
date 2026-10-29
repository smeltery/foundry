use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

#[derive(Clone)]
pub(crate) struct PermissionGate {
    state: Arc<Mutex<State>>,
    requested: Arc<AtomicU8>,
}

struct State {
    deadline: Option<Instant>,
    revoked: bool,
    generation: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateFailure {
    Uninitialized,
    Revoked,
    Expired,
    Stale,
    Poisoned,
}

impl PermissionGate {
    pub fn new(requested: Arc<AtomicU8>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                deadline: None,
                revoked: false,
                generation: 0,
            })),
            requested,
        }
    }

    /// Only the first lease owner initializes the clock; this cannot revive a gate.
    pub fn initialize(&self, deadline: Instant) {
        if let Ok(mut state) = self.state.lock() {
            state.deadline.get_or_insert(deadline);
        }
    }

    /// Called only by the watchdog after accepting a successful heartbeat update.
    pub fn renew(&self, previous: Instant, next: Instant) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let now = Instant::now();
        if state.deadline != Some(previous) || now >= previous || next <= now {
            state.revoked = true;
            return false;
        }
        state.deadline = Some(next);
        true
    }

    pub fn revoke(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.revoked = true;
        }
    }

    pub fn generation(&self) -> Result<u64, GateFailure> {
        self.state
            .lock()
            .map(|state| state.generation)
            .map_err(|_| GateFailure::Poisoned)
    }

    /// Policy changes invalidate outstanding authority under the same profile.
    pub fn invalidate(&self) {
        if let Ok(mut state) = self.state.lock() {
            match state.generation.checked_add(1) {
                Some(next) => state.generation = next,
                None => state.revoked = true,
            }
        }
    }

    /// The closure may only consume in-memory permission state, never perform I/O.
    /// Revocation and consumption linearize under this lock. A later stop still
    /// reaches the existing helper lifecycle through the runtime's stop flag.
    pub fn consume<T>(
        &self,
        generation: u64,
        consume: impl FnOnce() -> T,
    ) -> Result<T, GateFailure> {
        let mut state = self.state.lock().map_err(|_| GateFailure::Poisoned)?;
        if self.requested.load(Ordering::Acquire) != 0 {
            state.revoked = true;
        }
        if state.revoked {
            return Err(GateFailure::Revoked);
        }
        let deadline = state.deadline.ok_or(GateFailure::Uninitialized)?;
        if Instant::now() >= deadline {
            state.revoked = true;
            return Err(GateFailure::Expired);
        }
        if state.generation != generation {
            return Err(GateFailure::Stale);
        }
        Ok(consume())
    }
}

#[cfg(test)]
#[path = "permission_gate_tests.rs"]
mod tests;
