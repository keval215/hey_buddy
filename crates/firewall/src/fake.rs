//! Test fakes for the detector, session probe and screen-text source. There is deliberately NO fake `Firewall`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use frame::Rect;

use crate::{
    FirewallError, RawWindowTree, ScreenTextSource, SecureFieldDetector, SessionProbe,
    SessionProbeState,
};

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

/// What a fake text source was asked for. `ScreenTextSource` has a single, parameterless
/// request, so the log proves nothing but the foreground window is ever requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRequest {
    ForegroundWindow,
}

/// Scripted `ScreenTextSource`. Like the other fakes the last scripted result repeats.
/// It happily returns whatever tree it is given, including trees that violate the source
/// contract (secrets in secure nodes), so tests can prove the firewall does not rely on it.
pub struct FakeTextSource {
    script: Mutex<VecDeque<Result<RawWindowTree, FirewallError>>>,
    delay: Duration,
    panics: bool,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<TextRequest>>>,
}

impl FakeTextSource {
    pub fn new(tree: RawWindowTree) -> Self {
        Self::sequence(vec![Ok(tree)])
    }
    pub fn failing(e: FirewallError) -> Self {
        Self::sequence(vec![Err(e)])
    }
    pub fn sequence(results: Vec<Result<RawWindowTree, FirewallError>>) -> Self {
        assert!(!results.is_empty());
        Self {
            script: Mutex::new(results.into()),
            delay: Duration::ZERO,
            panics: false,
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
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
    /// Shared counter of started `foreground_tree` calls.
    pub fn call_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }
    /// Shared request log (usable after the fake is moved into a firewall).
    pub fn request_log(&self) -> Arc<Mutex<Vec<TextRequest>>> {
        Arc::clone(&self.requests)
    }
}

impl ScreenTextSource for FakeTextSource {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TextRequest::ForegroundWindow);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if self.panics {
            panic!("fake text source panic");
        }
        next(&self.script)
    }
}
