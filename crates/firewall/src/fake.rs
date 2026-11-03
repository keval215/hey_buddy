//! Test fakes for the detector and session probe. There is deliberately NO fake `Firewall`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use frame::Rect;

use crate::{FirewallError, SecureFieldDetector, SessionProbe, SessionProbeState};

/// Pops scripted results front to back; the last one repeats forever.
fn next<T: Clone>(q: &Mutex<VecDeque<T>>) -> T {
    let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() > 1 {
        g.pop_front().expect("len > 1")
    } else {
        g.front().expect("script is never empty").clone()
    }
}

pub struct FakeDetector {
    script: Mutex<VecDeque<Result<Vec<Rect>, FirewallError>>>,
    delay: Duration,
    delay_from_call: usize,
    calls: Arc<AtomicUsize>,
    panics: bool,
}

impl FakeDetector {
    /// Always returns `rects`.
    pub fn new(rects: Vec<Rect>) -> Self {
        Self::sequence(vec![Ok(rects)])
    }
    /// Always fails with `e` (use `IncompleteTree{..}` for the incomplete-tree case).
    pub fn failing(e: FirewallError) -> Self {
        Self::sequence(vec![Err(e)])
    }
    /// Successive calls return successive results; the last repeats (rects changing between calls).
    pub fn sequence(results: Vec<Result<Vec<Rect>, FirewallError>>) -> Self {
        assert!(!results.is_empty());
        Self {
            script: Mutex::new(results.into()),
            delay: Duration::ZERO,
            delay_from_call: 0,
            calls: Arc::new(AtomicUsize::new(0)),
            panics: false,
        }
    }
    pub fn with_delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
    /// Delay only calls with index >= `n` (0-based), e.g. `1` = fast preflight, slow check.
    pub fn with_delay_from_call(mut self, d: Duration, n: usize) -> Self {
        self.delay = d;
        self.delay_from_call = n;
        self
    }
    /// Shared counter of started `secure_fields` calls (usable after the fake is moved).
    pub fn call_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }
    pub fn panicking(mut self) -> Self {
        self.panics = true;
        self
    }
}

impl SecureFieldDetector for FakeDetector {
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() && n >= self.delay_from_call {
            std::thread::sleep(self.delay);
        }
        if self.panics {
            panic!("fake detector panic");
        }
        next(&self.script)
    }
}

pub struct FakeSessionProbe {
    script: Mutex<VecDeque<Result<SessionProbeState, FirewallError>>>,
    delay: Duration,
    panics: bool,
}

impl FakeSessionProbe {
    pub fn new(state: SessionProbeState) -> Self {
        Self::sequence(vec![Ok(state)])
    }
    pub fn normal() -> Self {
        Self::new(SessionProbeState::Normal)
    }
    pub fn failing(e: FirewallError) -> Self {
        Self::sequence(vec![Err(e)])
    }
    pub fn sequence(results: Vec<Result<SessionProbeState, FirewallError>>) -> Self {
        assert!(!results.is_empty());
        Self {
            script: Mutex::new(results.into()),
            delay: Duration::ZERO,
            panics: false,
        }
    }
    pub fn with_delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
    pub fn panicking(mut self) -> Self {
        self.panics = true;
        self
    }
}

impl SessionProbe for FakeSessionProbe {
    fn state(&self) -> Result<SessionProbeState, FirewallError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if self.panics {
            panic!("fake probe panic");
        }
        next(&self.script)
    }
}
