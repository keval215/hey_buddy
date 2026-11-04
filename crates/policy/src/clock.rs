//! Injectable time (ADR 0003 sections 6-7). The gate never reads the wall clock directly, so
//! timeouts, the 500 ms input guard, expiry and the rate floor are testable with [`ManualClock`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Monotonic time since an arbitrary origin.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;

    /// How long a blocked waiter should really sleep before it re-reads the clock, given the time
    /// `remaining` until its deadline. A decision wakes the waiter earlier (condvar).
    fn poll_slice(&self, remaining: Duration) -> Duration {
        remaining.clamp(Duration::from_millis(1), Duration::from_millis(200))
    }
}

/// The real monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that only moves when told to (tests and fakes).
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, d: Duration) {
        let mut g = self.now.lock().unwrap_or_else(|e| e.into_inner());
        *g = g.saturating_add(d);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn poll_slice(&self, _remaining: Duration) -> Duration {
        Duration::from_millis(1)
    }
}
