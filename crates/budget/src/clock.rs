//! Time sources and the validated wall-clock deadline.
use crate::error::BudgetError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Shortest nonzero wall-clock deadline: one minute.
pub const WALL_CLOCK_MIN_MS: u64 = 60_000;
/// Longest wall-clock deadline: one thousand hours.
pub const WALL_CLOCK_MAX_MS: u64 = 3_600_000_000;

/// Milliseconds on a time line shared by every process that opens the same
/// budget database, because admission times are persisted.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Milliseconds since the Unix epoch. Deadlines must survive a restart, so a
/// per-process monotonic origin cannot be used. A system clock stepped
/// backwards delays a deadline by the step; one stepped forward brings it closer.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    }
}

/// A clock that moves only when told to, for deterministic tests and fixtures.
#[derive(Default)]
pub struct ManualClock {
    now_ms: AtomicU64,
}

impl ManualClock {
    pub fn advance(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// A budget's wall-clock allowance, measured from execution admission. It covers
/// recovery waits and automatic continuations because it is plain elapsed time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallClock(Option<u64>);

impl WallClock {
    pub const NONE: Self = Self(None);

    /// Zero selects no deadline; otherwise 60,000 to 3,600,000,000 ms.
    pub fn from_ms(ms: u64) -> Result<Self, BudgetError> {
        match ms {
            0 => Ok(Self::NONE),
            WALL_CLOCK_MIN_MS..=WALL_CLOCK_MAX_MS => Ok(Self(Some(ms))),
            _ => Err(BudgetError::invalid("wall_clock_ms")),
        }
    }

    pub fn ms(&self) -> Option<u64> {
        self.0
    }
}
