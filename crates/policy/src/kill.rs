//! The kill switch (ADR 0003 section 9): a shared epoch counter. `trip` is one lock-free
//! `fetch_add`, so every `ApprovedAction::check_live` fails from that instant; registered
//! callbacks (the gate's `void_all`, later the driver's emergency release and the engine kill)
//! run after it. The OS hotkey thread, tray and TTS are S2-13; here only the type and semantics.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Why the switch was tripped. Fixed tokens only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    Hotkey,
    Tray,
    StopButton,
    /// The runaway/limit detector.
    Limit,
    Shutdown,
}

impl KillReason {
    pub fn as_str(self) -> &'static str {
        match self {
            KillReason::Hotkey => "hotkey",
            KillReason::Tray => "tray",
            KillReason::StopButton => "stop_button",
            KillReason::Limit => "limit",
            KillReason::Shutdown => "shutdown",
        }
    }
}

type Hook = Arc<dyn Fn(KillReason) + Send + Sync>;

/// Cheap to clone; all clones share the epoch.
#[derive(Clone)]
pub struct KillSwitch {
    epoch: Arc<AtomicU64>,
    hooks: Arc<Mutex<Vec<Hook>>>,
}

impl KillSwitch {
    pub fn new() -> Self {
        Self {
            epoch: Arc::new(AtomicU64::new(0)),
            hooks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Current epoch. Anything minted at an older epoch is dead.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// True if the switch was tripped after `epoch` was read.
    pub fn is_tripped_since(&self, epoch: u64) -> bool {
        self.epoch() != epoch
    }

    /// Trips the switch: the epoch moves first (lock-free), then the callbacks run. Returns the
    /// new epoch. Never blocks on the gate.
    pub fn trip(&self, reason: KillReason) -> u64 {
        let new = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let hooks: Vec<Hook> = self.hooks.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for h in hooks {
            h(reason);
        }
        new
    }

    /// Registers a callback run (on the tripping thread) after every trip.
    pub fn on_trip(&self, hook: Arc<dyn Fn(KillReason) + Send + Sync>) {
        self.hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(hook);
    }

    /// Moves the epoch WITHOUT running callbacks (tests: isolates the epoch condition).
    #[cfg(test)]
    pub(crate) fn bump_silently(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KillSwitch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KillSwitch(epoch {})", self.epoch())
    }
}
