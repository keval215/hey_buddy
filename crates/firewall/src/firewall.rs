use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frame::{AppDenyPolicy, Frame, Rect};

use crate::blackout::{blackout, DEFAULT_PADDING_PX};
use crate::builtin::{self, WithAuthSurfaces};
use crate::error::*;
use crate::observe::{
    AppId, Observation, ObservationFlags, ObservationId, ObserveVerdict, Slot, TargetSlot,
    WindowIdentity,
};
use crate::redacted::RedactedFrame;
use crate::target::{
    FocusGuard, FocusState, ForegroundFacts, LiveTarget, ObservationSource, ObservedWindow,
    ResolveRequest, ResolvedTarget, TargetKind, TargetRefusal, TargetResolver, TargetSel,
    UnsupportedObservationSource, UnsupportedTargetResolver, WindowFacts,
};
use crate::text::{
    redact_tree, redact_tree_indexed, RawWindowTree, ScreenTextSource, TextVerdict,
    UnsupportedTextSource, WithheldReason,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);
/// Default deadline for the foreground-window tree read of `check_text`.
pub const DEFAULT_TEXT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Default deadline for one target re-resolution / focus query of `revalidate`.
pub const DEFAULT_TARGET_TIMEOUT: Duration = Duration::from_millis(1000);
/// Maximum number of app ids listed in an observation.
const MAX_OBSERVED_APPS: usize = 32;
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
    /// Text-first screen context (ADR 0002 Addendum A): reads the foreground window's
    /// accessibility tree and returns firewall-redacted text.
    ///
    /// Consumes the preflight record exactly like `check`: ONE preflight authorises exactly ONE
    /// `check` OR ONE `check_text`, whichever runs first; a second call of either blocks.
    ///
    /// Default: fail closed, so a firewall without text support never yields text.
    fn check_text(&self, _deny: &dyn AppDenyPolicy) -> TextVerdict {
        TextVerdict::Blocked(BlockReason::Unsupported)
    }
    /// Agent observation (ADR 0004): the same pipeline as `check_text` (screen-text v1) plus a
    /// private target table and an `apps:` line. Consumes the preflight record like `check` and
    /// `check_text`: ONE preflight authorises exactly ONE of the three.
    ///
    /// Default: fail closed.
    fn observe(&self, _deny: &dyn AppDenyPolicy) -> ObserveVerdict {
        ObserveVerdict::Blocked(BlockReason::Unsupported)
    }
    /// Re-resolve one target of `obs` immediately before acting (ADR 0004 §6). Needs NO
    /// preflight: it returns no screen content, only a sealed, `!Send` handle. Every mismatch is
    /// a typed refusal; nothing is guessed or searched for.
    ///
    /// Default: fail closed.
    fn revalidate<'o>(
        &self,
        _obs: &'o Observation,
        _sel: TargetSel,
        _deny: &dyn AppDenyPolicy,
    ) -> Result<LiveTarget<'o>, TargetRefusal> {
        Err(TargetRefusal::Unsupported)
    }
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

fn probe_state_of(
    probe: &Arc<dyn SessionProbe>,
    busy: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<SessionProbeState, BlockReason> {
    let p = Arc::clone(probe);
    match with_deadline(busy, timeout, move || p.state()) {
        Deadline::Done(Ok(s)) => Ok(s),
        Deadline::Done(Err(e)) => Err(e.into()),
        Deadline::TimedOut | Deadline::Busy => Err(BlockReason::DetectionTimeout),
        Deadline::Panicked => Err(BlockReason::detection("session probe failed")),
    }
}

fn session_of(
    probe: &Arc<dyn SessionProbe>,
    busy: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<(), SessionState> {
    match probe_state_of(probe, busy, timeout) {
        Ok(SessionProbeState::Normal) => Ok(()),
        Ok(SessionProbeState::LockScreen) => Err(SessionState::Paused(PauseReason::LockScreen)),
        Ok(SessionProbeState::AuthPrompt) => Err(SessionState::Paused(PauseReason::AuthPrompt)),
        Ok(SessionProbeState::SecureInputActive) => {
            Err(SessionState::Paused(PauseReason::SecureInputActive))
        }
        Err(b) => Err(SessionState::Blocked(b)),
    }
}

fn refusal_of(s: SessionState) -> TargetRefusal {
    match s {
        SessionState::Paused(p) => TargetRefusal::SessionPaused(p),
        SessionState::Blocked(b) => TargetRefusal::Blocked(b),
        SessionState::Active => {
            TargetRefusal::Blocked(BlockReason::detection("firewall internal error"))
        }
    }
}

/// One resolver call on a deadline worker (timeout / busy / panic / error all fail closed).
fn resolver_call<T: Send + 'static>(
    resolver: &Arc<dyn TargetResolver>,
    busy: &Arc<AtomicBool>,
    timeout: Duration,
    f: impl FnOnce(&dyn TargetResolver) -> Result<T, FirewallError> + Send + 'static,
) -> Result<T, TargetRefusal> {
    let r = Arc::clone(resolver);
    match with_deadline(busy, timeout, move || f(&*r)) {
        Deadline::Done(Ok(v)) => Ok(v),
        Deadline::Done(Err(e)) => Err(TargetRefusal::Blocked(e.into())),
        Deadline::TimedOut | Deadline::Busy => {
            Err(TargetRefusal::Blocked(BlockReason::DetectionTimeout))
        }
        Deadline::Panicked => Err(TargetRefusal::Blocked(BlockReason::detection(
            "target resolver failed",
        ))),
    }
}

/// The foreground window must be the observed one (hwnd, then pid and process creation time,
/// then image path).
fn same_window(now: &WindowFacts, obs: &WindowFacts) -> Result<(), TargetRefusal> {
    if now.hwnd != obs.hwnd {
        return Err(TargetRefusal::ForegroundChanged);
    }
    if now.pid != obs.pid || now.process_creation_time != obs.process_creation_time {
        return Err(TargetRefusal::StaleRef);
    }
    if now.exe_path != obs.exe_path {
        return Err(TargetRefusal::TargetChanged { field: "app" });
    }
    Ok(())
}

/// Backs `LiveTarget::verify_focus`: session, foreground and focus checks on deadline workers.
struct FocusCtx {
    probe: Arc<dyn SessionProbe>,
    probe_busy: Arc<AtomicBool>,
    probe_timeout: Duration,
    resolver: Arc<dyn TargetResolver>,
    resolver_busy: Arc<AtomicBool>,
    target_timeout: Duration,
}

impl FocusGuard for FocusCtx {
    fn verify(
        &self,
        window: &WindowFacts,
        runtime_id: &[i32],
        within: bool,
    ) -> Result<(), TargetRefusal> {
        catch_unwind(AssertUnwindSafe(|| {
            session_of(&self.probe, &self.probe_busy, self.probe_timeout).map_err(refusal_of)?;
            let facts = resolver_call(
                &self.resolver,
                &self.resolver_busy,
                self.target_timeout,
                |r| r.foreground(),
            )?;
            if !facts.on_user_desktop {
                return Err(TargetRefusal::SessionPaused(PauseReason::AuthPrompt));
            }
            same_window(&facts.window, window)?;
            let hwnd = window.hwnd;
            let rid = runtime_id.to_vec();
            let state = resolver_call(
                &self.resolver,
                &self.resolver_busy,
                self.target_timeout,
                move |r| r.focus_state(hwnd, &rid),
            )?;
            match state {
                FocusState::Secure => Err(TargetRefusal::SecureTarget),
                FocusState::Target => Ok(()),
                FocusState::Descendant if within => Ok(()),
                _ => Err(TargetRefusal::FocusLost),
            }
        }))
        .unwrap_or(Err(TargetRefusal::Blocked(BlockReason::detection(
            "firewall internal error",
        ))))
    }
}

struct Record {
    rects: Vec<Rect>,
    taken: Instant,
}

/// Real firewall logic, generic over the detector, session probe and (optional) text source.
pub struct PrivacyFirewall<
    D: SecureFieldDetector,
    S: SessionProbe,
    T = UnsupportedTextSource,
    O = UnsupportedObservationSource,
    R = UnsupportedTargetResolver,
> {
    detector: Arc<D>,
    probe: Arc<S>,
    text_source: Arc<T>,
    text_timeout: Duration,
    text_busy: Arc<AtomicBool>,
    observation_source: Arc<O>,
    observe_busy: Arc<AtomicBool>,
    resolver: Arc<R>,
    resolver_busy: Arc<AtomicBool>,
    target_timeout: Duration,
    own_pid: u32,
    padding_px: u32,
    timeout: Duration,
    detector_timeout: Option<Duration>,
    max_record_age: Duration,
    detector_busy: Arc<AtomicBool>,
    probe_busy: Arc<AtomicBool>,
    /// Pre-capture record from the last successful `preflight`; consumed by `check` or
    /// `check_text`, whichever runs first.
    /// Concurrent preflights: the last writer wins; one record authorises one check.
    preflight_record: Mutex<Option<Record>>,
}

impl<D: SecureFieldDetector + 'static, S: SessionProbe + 'static>
    PrivacyFirewall<D, S, UnsupportedTextSource>
{
    pub fn new(detector: D, probe: S) -> Self {
        Self {
            detector: Arc::new(detector),
            probe: Arc::new(probe),
            text_source: Arc::new(UnsupportedTextSource),
            text_timeout: DEFAULT_TEXT_TIMEOUT,
            text_busy: Arc::new(AtomicBool::new(false)),
            observation_source: Arc::new(UnsupportedObservationSource),
            observe_busy: Arc::new(AtomicBool::new(false)),
            resolver: Arc::new(UnsupportedTargetResolver),
            resolver_busy: Arc::new(AtomicBool::new(false)),
            target_timeout: DEFAULT_TARGET_TIMEOUT,
            own_pid: std::process::id(),
            padding_px: DEFAULT_PADDING_PX,
            timeout: DEFAULT_TIMEOUT,
            detector_timeout: None,
            max_record_age: DEFAULT_MAX_RECORD_AGE,
            detector_busy: Arc::new(AtomicBool::new(false)),
            probe_busy: Arc::new(AtomicBool::new(false)),
            preflight_record: Mutex::new(None),
        }
    }
}

impl<
        D: SecureFieldDetector + 'static,
        S: SessionProbe + 'static,
        T: ScreenTextSource + 'static,
        O: ObservationSource + 'static,
        R: TargetResolver + 'static,
    > PrivacyFirewall<D, S, T, O, R>
{
    /// Install the foreground-tree reader used by `check_text`.
    pub fn with_text_source<T2: ScreenTextSource + 'static>(
        self,
        source: T2,
    ) -> PrivacyFirewall<D, S, T2, O, R> {
        PrivacyFirewall {
            detector: self.detector,
            probe: self.probe,
            text_source: Arc::new(source),
            text_timeout: self.text_timeout,
            text_busy: Arc::new(AtomicBool::new(false)),
            observation_source: self.observation_source,
            observe_busy: self.observe_busy,
            resolver: self.resolver,
            resolver_busy: self.resolver_busy,
            target_timeout: self.target_timeout,
            own_pid: self.own_pid,
            padding_px: self.padding_px,
            timeout: self.timeout,
            detector_timeout: self.detector_timeout,
            max_record_age: self.max_record_age,
            detector_busy: self.detector_busy,
            probe_busy: self.probe_busy,
            preflight_record: self.preflight_record,
        }
    }

    /// Install the readers used by `observe` (identity-carrying tree read, deadline
    /// `with_text_timeout`) and `revalidate` (deadline `with_target_timeout`).
    pub fn with_observation<O2: ObservationSource + 'static, R2: TargetResolver + 'static>(
        self,
        source: O2,
        resolver: R2,
    ) -> PrivacyFirewall<D, S, T, O2, R2> {
        PrivacyFirewall {
            detector: self.detector,
            probe: self.probe,
            text_source: self.text_source,
            text_timeout: self.text_timeout,
            text_busy: self.text_busy,
            observation_source: Arc::new(source),
            observe_busy: Arc::new(AtomicBool::new(false)),
            resolver: Arc::new(resolver),
            resolver_busy: Arc::new(AtomicBool::new(false)),
            target_timeout: self.target_timeout,
            own_pid: self.own_pid,
            padding_px: self.padding_px,
            timeout: self.timeout,
            detector_timeout: self.detector_timeout,
            max_record_age: self.max_record_age,
            detector_busy: self.detector_busy,
            probe_busy: self.probe_busy,
            preflight_record: self.preflight_record,
        }
    }

    /// Deadline for one target re-resolution or focus query (default 1000 ms).
    pub fn with_target_timeout(mut self, t: Duration) -> Self {
        self.target_timeout = t;
        self
    }

    /// Pretend our own process id is `pid` (tests only).
    #[cfg(any(test, feature = "fakes"))]
    pub fn with_own_pid(mut self, pid: u32) -> Self {
        self.own_pid = pid;
        self
    }

    /// Deadline for the foreground-tree read (default 1500 ms).
    pub fn with_text_timeout(mut self, t: Duration) -> Self {
        self.text_timeout = t;
        self
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

    fn probe_dyn(&self) -> Arc<dyn SessionProbe> {
        Arc::clone(&self.probe) as Arc<dyn SessionProbe>
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
        session_of(&self.probe_dyn(), &self.probe_busy, self.timeout)
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

    fn read_observed(&self) -> Result<ObservedWindow, BlockReason> {
        let src = Arc::clone(&self.observation_source);
        match with_deadline(&self.observe_busy, self.text_timeout, move || {
            src.observe_foreground()
        }) {
            Deadline::Done(Ok(t)) => Ok(t),
            Deadline::Done(Err(e)) => Err(e.into()),
            Deadline::TimedOut | Deadline::Busy => Err(BlockReason::DetectionTimeout),
            Deadline::Panicked => Err(BlockReason::detection("observation read failed")),
        }
    }

    fn observe_inner(&self, deny: &dyn AppDenyPolicy) -> ObserveVerdict {
        // 1. One preflight authorises exactly one check / check_text / observe.
        let record = self
            .preflight_record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // 2. Session probe.
        match self.session() {
            Ok(()) => {}
            Err(SessionState::Paused(p)) => return ObserveVerdict::Paused(p),
            Err(SessionState::Blocked(b)) => return ObserveVerdict::Blocked(b),
            Err(SessionState::Active) => {
                return ObserveVerdict::Blocked(BlockReason::detection("firewall internal error"))
            }
        }
        let Some(record) = record else {
            return ObserveVerdict::Blocked(BlockReason::UnstableLayout);
        };
        // 3. Foreground tree + identity on a deadline worker.
        let observed = match self.read_observed() {
            Ok(o) => o,
            Err(b) => return ObserveVerdict::Blocked(b),
        };
        if Instant::now().saturating_duration_since(record.taken) > self.max_record_age {
            return ObserveVerdict::Blocked(BlockReason::UnstableLayout);
        }
        let ObservedWindow {
            tree,
            identities,
            window,
            visible_apps,
        } = observed;
        if identities.len() != tree.nodes.len() {
            return ObserveVerdict::Blocked(BlockReason::detection("identity table mismatch"));
        }
        // Our own window is never observed; the user's deny list plus the built-in
        // authentication surfaces make an app invisible.
        let deny = WithAuthSurfaces(deny);
        let Some(app_id) = AppId::parse(&tree.app_id) else {
            return ObserveVerdict::Withheld(WithheldReason::DeniedApp);
        };
        if window.pid == self.own_pid {
            return ObserveVerdict::Withheld(WithheldReason::DeniedApp);
        }
        // Visible apps: app ids only, none denied / auth / ours.
        let mut apps: Vec<AppId> = Vec::new();
        for a in &visible_apps {
            if a.pid == self.own_pid || deny.is_denied(&a.app_id) {
                continue;
            }
            if let Some(id) = AppId::parse(&a.app_id) {
                if !apps.contains(&id) && apps.len() < MAX_OBSERVED_APPS {
                    apps.push(id);
                }
            }
        }
        let apps_line = (!apps.is_empty()).then(|| {
            let names: Vec<&str> = apps.iter().map(AppId::as_str).collect();
            format!("apps: {}\n", names.join(", "))
        });
        // 4-11: the same pass as check_text.
        let (verdict, index) =
            redact_tree_indexed(tree, &record.rects, &deny, apps_line.as_deref());
        let text = match verdict {
            TextVerdict::Clear(t) => t,
            TextVerdict::Withheld(w) => return ObserveVerdict::Withheld(w),
            TextVerdict::Paused(p) => return ObserveVerdict::Paused(p),
            TextVerdict::Blocked(b) => return ObserveVerdict::Blocked(b),
        };
        // Target table: secure nodes get NO slot; nodes without identity are not addressable;
        // ambiguous runtime ids are not addressable.
        let mut identities = identities;
        let mut counts = std::collections::HashMap::<Vec<i32>, u32>::new();
        for e in index.emitted.iter().filter(|e| !e.secure) {
            if let Some(Some(i)) = identities.get(e.idx) {
                *counts.entry(i.runtime_id.clone()).or_default() += 1;
            }
        }
        let slots: Vec<Slot> = index
            .emitted
            .into_iter()
            .map(|e| {
                if e.secure {
                    return Slot::Secure;
                }
                let ident = identities.get_mut(e.idx).and_then(Option::take);
                match (ident, e.rect) {
                    (Some(identity), Some(rect))
                        if !identity.runtime_id.is_empty()
                            && counts.get(&identity.runtime_id) == Some(&1) =>
                    {
                        Slot::Target(TargetSlot {
                            identity,
                            role: e.role,
                            editable: e.editable,
                            label: e.label.map(Arc::from),
                            rect,
                            sensitive_label: e.sensitive_label,
                        })
                    }
                    _ => Slot::Unaddressable,
                }
            })
            .collect();
        let Some(id) = ObservationId::fresh() else {
            return ObserveVerdict::Blocked(BlockReason::detection("observation id unavailable"));
        };
        let flags = ObservationFlags {
            payment_suspected: index.payment_suspected,
            secure_fields: text.secure_fields_hidden(),
        };
        ObserveVerdict::Clear(Observation::new(
            id,
            text,
            slots.into_boxed_slice(),
            WindowIdentity { facts: window },
            app_id,
            Arc::from(apps),
            flags,
        ))
    }

    fn revalidate_inner<'o>(
        &self,
        obs: &'o Observation,
        sel: TargetSel,
        deny: &dyn AppDenyPolicy,
    ) -> Result<LiveTarget<'o>, TargetRefusal> {
        let probe = self.probe_dyn();
        let resolver = Arc::clone(&self.resolver) as Arc<dyn TargetResolver>;
        let timeout = self.target_timeout;

        // 0. Which target? (No OS access.)
        let (kind, element, slot) = match sel {
            TargetSel::Element(r) => {
                if r.observation != obs.id() {
                    return Err(TargetRefusal::StaleRef);
                }
                match obs.slot(r.node) {
                    Some(Slot::Target(t)) => (TargetKind::Element, Some(r), Some(t)),
                    Some(Slot::Secure) => return Err(TargetRefusal::SecureTarget),
                    Some(Slot::Unaddressable) => return Err(TargetRefusal::NoSemanticPath),
                    None => return Err(TargetRefusal::StaleRef),
                }
            }
            TargetSel::ScrollRoot => (TargetKind::ScrollRoot, None, None),
            TargetSel::Window => (TargetKind::Window, None, None),
        };

        // 1. Session Normal.
        session_of(&probe, &self.probe_busy, self.probe_timeout()).map_err(refusal_of)?;

        // 2. Foreground window is the observed one; input desktop is the user's.
        let facts: ForegroundFacts =
            resolver_call(&resolver, &self.resolver_busy, timeout, |r| r.foreground())?;
        if !facts.on_user_desktop {
            return Err(TargetRefusal::SessionPaused(PauseReason::AuthPrompt));
        }
        let observed = &obs.window().facts;
        same_window(&facts.window, observed)?;
        if AppId::parse(&facts.app_id).as_ref() != Some(obs.app_id()) {
            return Err(TargetRefusal::TargetChanged { field: "app" });
        }

        // 3. Not ours, not an auth surface, not denied, not restricted, not elevated.
        if facts.window.pid == self.own_pid {
            return Err(TargetRefusal::OwnWindow);
        }
        if builtin::is_auth_surface(&facts.app_id) {
            return Err(TargetRefusal::AuthSurface);
        }
        if deny.is_denied(&facts.app_id) {
            return Err(TargetRefusal::DeniedApp);
        }
        if builtin::is_restricted(&facts.app_id) {
            return Err(TargetRefusal::RestrictedApp);
        }
        if facts.integrity > facts.own_integrity {
            return Err(TargetRefusal::ElevatedTarget);
        }

        // 4-6. Re-resolve inside that hwnd only; is_password first; identity must match exactly.
        let mut runtime_id = Vec::new();
        if let Some(t) = slot {
            runtime_id = t.identity.runtime_id.clone();
            let req = ResolveRequest {
                hwnd: observed.hwnd,
                runtime_id: runtime_id.clone(),
            };
            let found = match resolver_call(&resolver, &self.resolver_busy, timeout, move |r| {
                r.resolve(&req)
            })? {
                ResolvedTarget::Gone => return Err(TargetRefusal::StaleRef),
                ResolvedTarget::Secure => return Err(TargetRefusal::SecureTarget),
                ResolvedTarget::Found(f) => f,
            };
            if found.control_type != t.identity.control_type {
                return Err(TargetRefusal::TargetChanged {
                    field: "control_type",
                });
            }
            if found.automation_id_hash != t.identity.automation_id_hash {
                return Err(TargetRefusal::TargetChanged {
                    field: "automation_id",
                });
            }
            if found.label_hash != t.identity.label_hash {
                return Err(TargetRefusal::TargetChanged { field: "label" });
            }
            if found.rect != t.rect {
                return Err(TargetRefusal::TargetChanged { field: "rect" });
            }
        }

        // Heuristic refusals (lexicons): payment pages allow only scrolling; secret-named text
        // targets are never typed into.
        if kind != TargetKind::ScrollRoot && obs.flags().payment_suspected {
            return Err(TargetRefusal::PaymentSuspected);
        }
        if let Some(t) = slot {
            let text_target = t.editable
                || matches!(
                    t.role,
                    crate::text::NodeRole::TextField | crate::text::NodeRole::ComboBox
                );
            if text_target && t.sensitive_label {
                return Err(TargetRefusal::SensitiveTarget);
            }
        }

        let guard = Arc::new(FocusCtx {
            probe,
            probe_busy: Arc::clone(&self.probe_busy),
            probe_timeout: self.probe_timeout(),
            resolver,
            resolver_busy: Arc::clone(&self.resolver_busy),
            target_timeout: timeout,
        });
        Ok(LiveTarget::new(
            obs,
            element,
            kind,
            observed.clone(),
            runtime_id,
            guard,
        ))
    }

    fn probe_timeout(&self) -> Duration {
        self.timeout
    }

    fn read_tree(&self) -> Result<RawWindowTree, BlockReason> {
        let src = Arc::clone(&self.text_source);
        match with_deadline(&self.text_busy, self.text_timeout, move || {
            src.foreground_tree()
        }) {
            Deadline::Done(Ok(t)) => Ok(t),
            Deadline::Done(Err(e)) => Err(e.into()),
            Deadline::TimedOut | Deadline::Busy => Err(BlockReason::DetectionTimeout),
            Deadline::Panicked => Err(BlockReason::detection("screen text read failed")),
        }
    }

    fn check_text_inner(&self, deny: &dyn AppDenyPolicy) -> TextVerdict {
        // 1. Always consume the record: one preflight authorises exactly one check/check_text.
        let record = self
            .preflight_record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // 2. Session probe.
        match self.session() {
            Ok(()) => {}
            Err(SessionState::Paused(p)) => return TextVerdict::Paused(p),
            Err(SessionState::Blocked(b)) => return TextVerdict::Blocked(b),
            Err(SessionState::Active) => {
                return TextVerdict::Blocked(BlockReason::detection("firewall internal error"))
            }
        }
        let Some(record) = record else {
            return TextVerdict::Blocked(BlockReason::UnstableLayout);
        };
        // 3. Foreground tree on a deadline worker.
        let tree = match self.read_tree() {
            Ok(t) => t,
            Err(b) => return TextVerdict::Blocked(b),
        };
        // The record must still be fresh at the time the tree was read.
        if Instant::now().saturating_duration_since(record.taken) > self.max_record_age {
            return TextVerdict::Blocked(BlockReason::UnstableLayout);
        }
        // 4-11.
        redact_tree(tree, &record.rects, deny)
    }
}

impl<
        D: SecureFieldDetector + 'static,
        S: SessionProbe + 'static,
        T: ScreenTextSource + 'static,
        O: ObservationSource + 'static,
        R: TargetResolver + 'static,
    > Firewall for PrivacyFirewall<D, S, T, O, R>
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

    fn check_text(&self, deny: &dyn AppDenyPolicy) -> TextVerdict {
        catch_unwind(AssertUnwindSafe(|| self.check_text_inner(deny))).unwrap_or_else(|_| {
            TextVerdict::Blocked(BlockReason::detection("firewall internal error"))
        })
    }

    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveVerdict {
        catch_unwind(AssertUnwindSafe(|| self.observe_inner(deny))).unwrap_or_else(|_| {
            ObserveVerdict::Blocked(BlockReason::detection("firewall internal error"))
        })
    }

    fn revalidate<'o>(
        &self,
        obs: &'o Observation,
        sel: TargetSel,
        deny: &dyn AppDenyPolicy,
    ) -> Result<LiveTarget<'o>, TargetRefusal> {
        catch_unwind(AssertUnwindSafe(|| self.revalidate_inner(obs, sel, deny))).unwrap_or(Err(
            TargetRefusal::Blocked(BlockReason::detection("firewall internal error")),
        ))
    }
}
