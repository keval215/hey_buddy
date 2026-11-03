use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frame::{Frame, Rect};

use crate::blackout::{blackout, DEFAULT_PADDING_PX};
use crate::error::*;
use crate::redacted::RedactedFrame;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);
/// Default maximum age of a preflight record (and maximum gap between record and capture).
pub const DEFAULT_MAX_RECORD_AGE: Duration = Duration::from_secs(2);

/// Finds secure (password) fields on ALL visible windows of ALL displays.
///
/// The contract is display-INDEPENDENT: the returned rects are in the global coordinate space
/// that `capture` uses for `DisplayInfo.origin`/`scale` (macOS: points; Windows: physical
/// virtual-desktop pixels with `scale == 1.0`), and must be identical whenever the screen is
/// unchanged, so the pre-capture and post-capture queries compare equal. A secure field with
/// no usable (finite, non-zero-area) rect is an `Err`, never an empty rect. Rects entirely
/// outside the captured display are fine: the blackout ignores them (no pixels to hide).
pub trait SecureFieldDetector: Send + Sync {
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError>;
}

/// Detects lock screen, auth prompts and secure keyboard input.
pub trait SessionProbe: Send + Sync {
    fn state(&self) -> Result<SessionProbeState, FirewallError>;
}

pub enum Verdict {
    Clear(RedactedFrame),
    Paused(PauseReason),
    Blocked(BlockReason),
}

impl std::fmt::Debug for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Clear(r) => write!(f, "Clear({r:?})"),
            Verdict::Paused(p) => write!(f, "Paused({p:?})"),
            Verdict::Blocked(b) => write!(f, "Blocked({b:?})"),
        }
    }
}

pub trait Firewall: Send + Sync {
    /// Call BEFORE capture. Records the pre-capture secure-field set inside the firewall.
    fn preflight(&self) -> SessionState;
    /// Call with the captured frame. Takes the frame by value: the caller keeps no raw pixels.
    fn check(&self, frame: Frame) -> Verdict;
}

enum Deadline<T> {
    Done(T),
    TimedOut,
    /// A previous (timed-out) call is still running; nothing was spawned.
    Busy,
    Panicked,
}

/// Clears the in-flight flag when dropped (also on panic / failed spawn).
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Run `f` on a worker thread and wait at most `timeout`. A hung worker is abandoned, but
/// `guard` stays set until it finishes, so at most one worker per guard exists at a time.
fn with_deadline<T: Send + 'static>(
    guard: &Arc<AtomicBool>,
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Deadline<T> {
    if guard
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Deadline::Busy;
    }
    let release = InFlight(Arc::clone(guard));
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("firewall-detect".into())
        .spawn(move || {
            let v = f();
            // Release before sending so a caller that sees the result never sees "busy".
            drop(release);
            let _ = tx.send(v);
        });
    if spawned.is_err() {
        // The closure (and its guard) was dropped, which cleared the flag.
        return Deadline::Panicked;
    }
    match rx.recv_timeout(timeout) {
        Ok(v) => Deadline::Done(v),
        Err(RecvTimeoutError::Timeout) => Deadline::TimedOut,
        // Sender dropped without sending: the worker panicked.
        Err(RecvTimeoutError::Disconnected) => Deadline::Panicked,
    }
}

fn normalize(mut v: Vec<Rect>) -> Vec<Rect> {
    v.sort_by_key(|r| (r.x, r.y, r.w, r.h));
    v.dedup();
    v
}

struct Record {
    rects: Vec<Rect>,
    taken: Instant,
}

/// Real firewall logic, generic over the detector and session probe.
pub struct PrivacyFirewall<D: SecureFieldDetector, S: SessionProbe> {
    detector: Arc<D>,
    probe: Arc<S>,
    padding_px: u32,
    timeout: Duration,
    detector_timeout: Option<Duration>,
    max_record_age: Duration,
    detector_busy: Arc<AtomicBool>,
    probe_busy: Arc<AtomicBool>,
    /// Pre-capture record from the last successful `preflight`; consumed by `check`.
    /// Concurrent preflights: the last writer wins; one record authorises one check.
    preflight_record: Mutex<Option<Record>>,
}

impl<D: SecureFieldDetector + 'static, S: SessionProbe + 'static> PrivacyFirewall<D, S> {
    pub fn new(detector: D, probe: S) -> Self {
        Self {
            detector: Arc::new(detector),
            probe: Arc::new(probe),
            padding_px: DEFAULT_PADDING_PX,
            timeout: DEFAULT_TIMEOUT,
            detector_timeout: None,
            max_record_age: DEFAULT_MAX_RECORD_AGE,
            detector_busy: Arc::new(AtomicBool::new(false)),
            probe_busy: Arc::new(AtomicBool::new(false)),
            preflight_record: Mutex::new(None),
        }
    }

    pub fn with_padding(mut self, px: u32) -> Self {
        self.padding_px = px;
        self
    }

    /// Deadline for the session probe (and for the detector unless overridden).
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Separate, usually longer, deadline for the secure-field detector (UI tree walks are
    /// slower than the session probe).
    pub fn with_detector_timeout(mut self, t: Duration) -> Self {
        self.detector_timeout = Some(t);
        self
    }

    /// Maximum age of the preflight record, and maximum gap between the record and the frame's
    /// `captured_at`. Older => `Blocked(UnstableLayout)`.
    pub fn with_max_record_age(mut self, t: Duration) -> Self {
        self.max_record_age = t;
        self
    }

    fn clear_record(&self) {
        // A poisoned lock still lets us overwrite; never leave a stale record.
        let mut g = self
            .preflight_record
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *g = None;
    }

    fn probe_state(&self) -> Result<SessionProbeState, BlockReason> {
        let p = Arc::clone(&self.probe);
        match with_deadline(&self.probe_busy, self.timeout, move || p.state()) {
            Deadline::Done(Ok(s)) => Ok(s),
            Deadline::Done(Err(e)) => Err(e.into()),
            Deadline::TimedOut | Deadline::Busy => Err(BlockReason::DetectionTimeout),
            Deadline::Panicked => Err(BlockReason::detection("session probe failed")),
        }
    }

    fn query(&self) -> Result<Vec<Rect>, BlockReason> {
        let d = Arc::clone(&self.detector);
        let t = self.detector_timeout.unwrap_or(self.timeout);
        match with_deadline(&self.detector_busy, t, move || d.secure_fields()) {
            Deadline::Done(Ok(v)) => {
                // A secure field without area cannot be hidden or compared: fail closed.
                if v.iter().any(|r| r.is_empty()) {
                    return Err(BlockReason::detection("secure field has no usable area"));
                }
                Ok(normalize(v))
            }
            Deadline::Done(Err(e)) => Err(e.into()),
            Deadline::TimedOut | Deadline::Busy => Err(BlockReason::DetectionTimeout),
            Deadline::Panicked => Err(BlockReason::detection("secure-field detection failed")),
        }
    }

    fn session(&self) -> Result<(), SessionState> {
        match self.probe_state() {
            Ok(SessionProbeState::Normal) => Ok(()),
            Ok(SessionProbeState::LockScreen) => Err(SessionState::Paused(PauseReason::LockScreen)),
            Ok(SessionProbeState::AuthPrompt) => Err(SessionState::Paused(PauseReason::AuthPrompt)),
            Ok(SessionProbeState::SecureInputActive) => {
                Err(SessionState::Paused(PauseReason::SecureInputActive))
            }
            Err(b) => Err(SessionState::Blocked(b)),
        }
    }

    fn preflight_inner(&self) -> SessionState {
        self.clear_record();
        if let Err(s) = self.session() {
            return s;
        }
        match self.query() {
            Ok(rects) => {
                let mut g = self
                    .preflight_record
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Stamped when preflight finishes, i.e. before the caller captures.
                *g = Some(Record {
                    rects,
                    taken: Instant::now(),
                });
                SessionState::Active
            }
            Err(b) => SessionState::Blocked(b),
        }
    }

    fn record_is_fresh(&self, taken: Instant, captured_at: Instant) -> bool {
        // The record must be taken BEFORE capture and not too long before it.
        match captured_at.checked_duration_since(taken) {
            Some(gap) if gap <= self.max_record_age => {}
            _ => return false,
        }
        Instant::now().saturating_duration_since(taken) <= self.max_record_age
    }

    fn check_inner(&self, mut frame: Frame) -> Verdict {
        // Always consume the record: one preflight authorises exactly one check.
        let record = self
            .preflight_record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match self.session() {
            Ok(()) => {}
            Err(SessionState::Paused(p)) => return Verdict::Paused(p),
            Err(SessionState::Blocked(b)) => return Verdict::Blocked(b),
            Err(SessionState::Active) => {
                return Verdict::Blocked(BlockReason::detection("firewall internal error"))
            }
        }
        let Some(record) = record else {
            return Verdict::Blocked(BlockReason::detection("no preflight record"));
        };
        if !self.record_is_fresh(record.taken, frame.captured_at()) {
            return Verdict::Blocked(BlockReason::UnstableLayout);
        }
        let after = match self.query() {
            Ok(r) => r,
            Err(b) => return Verdict::Blocked(b),
        };
        if after != record.rects {
            return Verdict::Blocked(BlockReason::UnstableLayout);
        }
        match blackout(&mut frame, &after, self.padding_px) {
            Some(applied) => Verdict::Clear(RedactedFrame::from_blackout(frame, applied)),
            None => Verdict::Blocked(BlockReason::detection("blackout failed")),
        }
    }
}

impl<D: SecureFieldDetector + 'static, S: SessionProbe + 'static> Firewall
    for PrivacyFirewall<D, S>
{
    fn preflight(&self) -> SessionState {
        catch_unwind(AssertUnwindSafe(|| self.preflight_inner())).unwrap_or_else(|_| {
            self.clear_record();
            SessionState::Blocked(BlockReason::detection("firewall internal error"))
        })
    }

    fn check(&self, frame: Frame) -> Verdict {
        catch_unwind(AssertUnwindSafe(|| self.check_inner(frame)))
            .unwrap_or_else(|_| Verdict::Blocked(BlockReason::detection("firewall internal error")))
    }
}
