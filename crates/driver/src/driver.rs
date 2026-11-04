//! `Driver<A>`: the orchestration of ADR 0004 (sections 6-10, 12, 13). See `docs/driver.md`.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use action::{ActionKind, KeyCombo, ScrollAmount, ScrollDirection, ScrollTarget, TypedText};
use firewall::{
    AppId, BlockReason, ElementRef, Firewall, LiveTarget, NodeRole, Observation, ObservationFlags,
    ObserveVerdict, PauseReason, SessionState, TargetDescriptor, TargetSel, WithheldReason,
};
use frame::AppDenyPolicy;
use policy::{ApprovedAction, Clock, KillSwitch, SystemClock};

use crate::actuator::{Actuator, ClickMethod, MAX_CHUNK_CHARS};
use crate::error::{ActionReceipt, Delivery, DriverError, ExecOutcome};
use crate::limits::{DenySnapshot, LaunchAllowlist, Limiter, CALL_DEADLINE, OBSERVATION_MAX_AGE};
use crate::screen::ObservedScreen;

/// Result of `DesktopDriver::observe`.
#[derive(Debug)]
pub enum ObserveOutcome {
    Clear(ObservedScreen),
    Withheld(WithheldReason),
    Paused(PauseReason),
    Blocked(BlockReason),
}

/// The only way to act. Sync like `Firewall`: core calls it from `spawn_blocking`.
pub trait DesktopDriver: Send + Sync {
    /// Preflight + `Firewall::observe`; caches the observation (one execute can spend it).
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome;
    /// Descriptor of an element of the cached, unspent observation (never a second reader).
    fn describe(&self, r: ElementRef) -> Result<TargetDescriptor, DriverError>;
    /// Performs one gate-minted action. Consumes the approval and spends the observation.
    ///
    /// ```compile_fail,E0308
    /// // An `Action` is data, not authority.
    /// fn f(
    ///     d: &dyn driver::DesktopDriver,
    ///     a: action::Action,
    ///     deny: &dyn frame::AppDenyPolicy,
    ///     k: &policy::KillSwitch,
    /// ) {
    ///     d.execute(a, deny, k);
    /// }
    /// ```
    /// ```compile_fail,E0382
    /// // Single use: the approval is consumed by value.
    /// fn f(
    ///     d: &dyn driver::DesktopDriver,
    ///     a: policy::ApprovedAction,
    ///     deny: &dyn frame::AppDenyPolicy,
    ///     k: &policy::KillSwitch,
    /// ) {
    ///     d.execute(a, deny, k);
    ///     d.execute(a, deny, k);
    /// }
    /// ```
    /// ```compile_fail,E0277
    /// // A `LiveTarget` is `!Send`: it cannot leave the thread that revalidated it.
    /// fn leak(t: firewall::LiveTarget<'static>) {
    ///     std::thread::spawn(move || {
    ///         let _t = t;
    ///     });
    /// }
    /// ```
    fn execute(
        &self,
        action: ApprovedAction,
        deny: &dyn AppDenyPolicy,
        kill: &KillSwitch,
    ) -> ExecOutcome;
    /// Release the keys and buttons the driver itself pressed and stop any `execute` that is
    /// already running (it refuses its next primitive). Idempotent; needs no `ApprovedAction`.
    /// An `execute` that starts afterwards is not affected (it needs a fresh approval anyway).
    fn emergency_release(&self);
}

struct Latest {
    obs: Arc<Observation>,
    spent: bool,
    cached_at: Duration,
}

/// Messages from the worker thread.
enum Msg {
    /// The worker is about to make (or just made) a call: restarts the caller's deadline.
    Beat,
    Done(Raw),
}

enum Raw {
    Ok(Delivery),
    Refused(DriverError),
    Failed {
        error: DriverError,
        delivery: Delivery,
    },
}

/// State shared between the caller and its worker.
#[derive(Default)]
pub(crate) struct Shared {
    /// The caller gave up (timeout): the worker must not make another call.
    pub(crate) abandoned: AtomicBool,
    /// An input-affecting actuator call was started.
    pub(crate) acted: AtomicBool,
    /// A `send_chars` call was started (it may have typed something even if it failed or hung).
    pub(crate) chars_started: AtomicBool,
    /// Characters whose `send_chars` returned `Ok`.
    pub(crate) sent: AtomicU32,
}

/// Clears the in-flight flag when dropped (the worker returned, panicked or failed to spawn).
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// What to do, fully decided (and statically checked) on the caller thread.
pub(crate) enum Op {
    Focus(ElementRef),
    Click(ElementRef, ClickMethod),
    Scroll {
        sel: TargetSel,
        dir: ScrollDirection,
        amount: ScrollAmount,
        repeat: u8,
    },
    SetText(ElementRef, TypedText),
    TypeText(ElementRef, TypedText),
    Key(ElementRef, KeyCombo),
    Copy(ElementRef),
    Launch(PathBuf),
    FocusApp(AppId),
}

impl Op {
    fn types(&self) -> bool {
        matches!(self, Op::TypeText(..))
    }
}

struct Job<A> {
    firewall: Arc<dyn Firewall>,
    actuator: Arc<A>,
    obs: Arc<Observation>,
    deny: DenySnapshot,
    kill: KillSwitch,
    epoch0: u64,
    release_gen: Arc<AtomicU64>,
    /// `release_gen` when this execute took the worker slot: a different value means an
    /// `emergency_release` happened since.
    gen0: u64,
    shared: Arc<Shared>,
    approved: ApprovedAction,
    op: Op,
    tx: Sender<Msg>,
}

impl<A: Actuator> Job<A> {
    fn beat(&self) {
        let _ = self.tx.send(Msg::Beat);
    }

    /// Everything that must hold immediately before an input primitive or a revalidation:
    /// not abandoned, no `emergency_release` since this execute began, kill switch not tripped,
    /// approval still live.
    fn gate(&self) -> Result<(), DriverError> {
        self.beat();
        if self.shared.abandoned.load(Ordering::SeqCst) {
            return Err(DriverError::Timeout);
        }
        if self.released() || self.kill.is_tripped_since(self.epoch0) {
            return Err(DriverError::Killed);
        }
        self.approved.check_live().map_err(|_| DriverError::Revoked)
    }

    fn released(&self) -> bool {
        self.release_gen.load(Ordering::SeqCst) != self.gen0
    }

    /// Kill check, then the firewall re-resolves the target. The `LiveTarget` is created here, on
    /// the worker, and never leaves it.
    fn revalidate(&self, sel: TargetSel) -> Result<LiveTarget<'_>, DriverError> {
        self.gate()?;
        let live = self
            .firewall
            .revalidate(&self.obs, sel, &self.deny)
            .map_err(DriverError::from)?;
        Ok(live)
    }

    /// One input primitive: gates, marks the action as started, calls the actuator, and if the
    /// driver was released while the call ran, releases whatever it pressed.
    fn act(
        &self,
        f: impl FnOnce(&A) -> Result<(), crate::error::ActError>,
    ) -> Result<(), DriverError> {
        self.gate()?;
        self.shared.acted.store(true, Ordering::SeqCst);
        let r = f(self.actuator.as_ref());
        self.beat();
        if self.released() {
            self.actuator.release_all();
        }
        r.map_err(DriverError::from)
    }

    fn verify(&self, live: &LiveTarget<'_>, within: bool) -> Result<(), DriverError> {
        self.beat();
        let r = if within {
            live.verify_focus_within()
        } else {
            live.verify_focus()
        };
        r.map_err(DriverError::from)
    }

    fn run(&self) -> Raw {
        match self.steps() {
            Ok(()) => Raw::Ok(Delivery::DeliveredUnverified),
            Err(e) => self.stop(e),
        }
    }

    fn stop(&self, error: DriverError) -> Raw {
        let (delivery, started) = failure_delivery(&self.shared, self.op.types(), &error);
        if started {
            Raw::Failed { error, delivery }
        } else {
            Raw::Refused(error)
        }
    }

    fn steps(&self) -> Result<(), DriverError> {
        match &self.op {
            Op::Focus(el) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.act(|a| a.set_focus(&live))
            }
            Op::Click(el, m) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.act(|a| a.invoke(&live, *m))
            }
            Op::Scroll {
                sel,
                dir,
                amount,
                repeat,
            } => {
                let live = self.revalidate(*sel)?;
                for _ in 0..*repeat {
                    self.act(|a| a.scroll(&live, *dir, *amount))?;
                }
                Ok(())
            }
            Op::SetText(el, text) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.act(|a| a.set_value(&live, text))
            }
            Op::TypeText(el, text) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.act(|a| a.set_focus(&live))?;
                self.verify(&live, false)?;
                let chars: Vec<char> = text.as_str().chars().collect();
                for chunk in chars.chunks(MAX_CHUNK_CHARS) {
                    self.gate()?;
                    self.verify(&live, false)?;
                    let s: String = chunk.iter().collect();
                    self.act(|a| {
                        self.shared.chars_started.store(true, Ordering::SeqCst);
                        a.send_chars(&s)
                    })?;
                    self.shared
                        .sent
                        .fetch_add(chunk.len() as u32, Ordering::SeqCst);
                }
                // The last chunk went to the right place only if focus is still there.
                self.gate()?;
                self.verify(&live, false)
            }
            Op::Key(el, k) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.gate()?;
                // Backspace/Delete edit the target itself: focus must be exactly there. Other keys
                // (navigation) may be sent while a descendant of the target holds focus.
                self.verify(&live, !k.needs_editable_target())?;
                self.act(|a| a.send_key(*k))
            }
            Op::Copy(el) => {
                let live = self.revalidate(TargetSel::Element(*el))?;
                self.gate()?;
                self.verify(&live, true)?;
                self.act(|a| a.send_copy())
            }
            Op::Launch(path) => {
                let _live = self.revalidate(TargetSel::Window)?;
                self.act(|a| a.launch(path))
            }
            Op::FocusApp(app) => {
                let _live = self.revalidate(TargetSel::Window)?;
                self.act(|a| a.focus_app(app))
            }
        }
    }
}

/// `(delivery, started)`: whether an input call was started and what is known about delivery.
pub(crate) fn failure_delivery(
    shared: &Shared,
    typing: bool,
    error: &DriverError,
) -> (Delivery, bool) {
    let started = shared.acted.load(Ordering::SeqCst);
    let sent = shared.sent.load(Ordering::SeqCst);
    let d = if typing {
        if sent > 0 {
            // A lower bound: a later chunk may have been typed without reporting success.
            Delivery::Partial { chars_sent: sent }
        } else if shared.chars_started.load(Ordering::SeqCst) {
            // The first `send_chars` failed or hung: it may still have typed something.
            Delivery::DeliveredUnverified
        } else {
            Delivery::NotDelivered
        }
    } else if matches!(error, DriverError::Unsupported(_)) {
        Delivery::NotDelivered
    } else {
        Delivery::DeliveredUnverified
    };
    (d, started)
}

/// The desktop driver. Generic over the platform [`Actuator`].
pub struct Driver<A: Actuator + 'static> {
    firewall: Arc<dyn Firewall>,
    actuator: Arc<A>,
    latest: Mutex<Option<Latest>>,
    launch: LaunchAllowlist,
    limiter: Mutex<Limiter>,
    clock: Arc<dyn Clock>,
    busy: Arc<AtomicBool>,
    /// Bumped by every `emergency_release`; an `execute` compares it with its own snapshot.
    release_gen: Arc<AtomicU64>,
    observe_lock: Mutex<()>,
    call_deadline: Duration,
    #[cfg(feature = "fakes")]
    slot_hook: Mutex<Option<SlotHook>>,
}

/// Test hook (feature `fakes`).
#[cfg(feature = "fakes")]
type SlotHook = Arc<dyn Fn() + Send + Sync>;

impl<A: Actuator + 'static> Driver<A> {
    pub fn new(firewall: Arc<dyn Firewall>, actuator: A) -> Self {
        Self {
            firewall,
            actuator: Arc::new(actuator),
            latest: Mutex::new(None),
            launch: LaunchAllowlist::windows_default(),
            limiter: Mutex::new(Limiter::default()),
            clock: Arc::new(SystemClock::new()),
            busy: Arc::new(AtomicBool::new(false)),
            release_gen: Arc::new(AtomicU64::new(0)),
            observe_lock: Mutex::new(()),
            call_deadline: CALL_DEADLINE,
            #[cfg(feature = "fakes")]
            slot_hook: Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_launch_allowlist(mut self, l: LaunchAllowlist) -> Self {
        self.launch = l;
        self
    }

    /// Deadline for one call without progress (default 3 s).
    pub fn with_call_deadline(mut self, d: Duration) -> Self {
        self.call_deadline = d;
        self
    }

    /// Number of `emergency_release` calls so far (a counter, never cleared).
    pub fn release_generation(&self) -> u64 {
        self.release_gen.load(Ordering::SeqCst)
    }

    /// Test hook (feature `fakes`): runs on the caller thread right after `execute` took the worker
    /// slot and snapshotted the release generation, before the worker starts.
    #[cfg(feature = "fakes")]
    pub fn set_slot_hook(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.slot_hook) = Some(Arc::new(f));
    }

    /// Test hook (feature `fakes`): replaces the cached, unspent observation with `obs` (which a
    /// test has given the cached one's id with `Observation::with_id`), so step (e) sees a screen
    /// that differs from the one the approval was bound to. `false` when nothing is cached.
    #[cfg(feature = "fakes")]
    pub fn replace_cached_observation(&self, obs: Observation) -> bool {
        match lock(&self.latest).as_mut() {
            Some(l) => {
                l.obs = Arc::new(obs);
                true
            }
            None => false,
        }
    }

    /// A previous (timed-out) call is still running on the worker.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Runs `f` on the cached, unspent observation (`None` when there is none). For core's
    /// `policy::bind_step`; the observation is not otherwise exposed.
    pub fn with_latest<R>(&self, f: impl FnOnce(&Observation) -> R) -> Option<R> {
        let obs = {
            let g = lock(&self.latest);
            let l = g.as_ref().filter(|l| !l.spent)?;
            Arc::clone(&l.obs)
        };
        Some(f(&obs))
    }

    fn run_on_worker(
        &self,
        approved: ApprovedAction,
        op: Op,
        obs: Arc<Observation>,
        deny: DenySnapshot,
        kill: &KillSwitch,
        epoch0: u64,
    ) -> ExecOutcome {
        let digest = *approved.digest().as_bytes();
        let observation = obs.id();
        // One worker at a time: a hung call keeps the driver Busy until it returns.
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return ExecOutcome::Refused(DriverError::Busy);
        }
        let guard = BusyGuard(Arc::clone(&self.busy));
        // No worker is running. A release after this snapshot stops this execute at its next
        // gate; one before it concerned an earlier execute. (Nothing is ever "cleared".)
        let gen0 = self.release_gen.load(Ordering::SeqCst);
        #[cfg(feature = "fakes")]
        {
            let hook = lock(&self.slot_hook).clone();
            if let Some(h) = hook {
                h();
            }
        }
        let typing = op.types();
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel();
        let job = Job {
            firewall: Arc::clone(&self.firewall),
            actuator: Arc::clone(&self.actuator),
            obs,
            deny,
            kill: kill.clone(),
            epoch0,
            release_gen: Arc::clone(&self.release_gen),
            gen0,
            shared: Arc::clone(&shared),
            approved,
            op,
            tx: tx.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("driver-worker".into())
            .spawn(move || {
                let raw = catch_unwind(AssertUnwindSafe(|| job.run())).unwrap_or_else(|_| {
                    // An input call died half way: let go of whatever it pressed.
                    let _ = catch_unwind(AssertUnwindSafe(|| job.actuator.release_all()));
                    job.stop(DriverError::Platform("worker panicked"))
                });
                // Free the slot BEFORE reporting: a caller that has its result sees a free driver.
                drop(guard);
                let _ = tx.send(Msg::Done(raw));
            });
        if spawned.is_err() {
            return ExecOutcome::Refused(DriverError::Platform("worker spawn failed"));
        }
        loop {
            match rx.recv_timeout(self.call_deadline) {
                Ok(Msg::Beat) => {}
                Ok(Msg::Done(raw)) => {
                    return match raw {
                        Raw::Ok(delivery) => ExecOutcome::Executed(ActionReceipt {
                            digest,
                            observation,
                            delivery,
                        }),
                        Raw::Refused(e) => ExecOutcome::Refused(e),
                        Raw::Failed { error, delivery } => ExecOutcome::Failed { error, delivery },
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    shared.abandoned.store(true, Ordering::SeqCst);
                    let (d, started) = failure_delivery(&shared, typing, &DriverError::Timeout);
                    let delivery = if started { d } else { Delivery::NotDelivered };
                    return ExecOutcome::Failed {
                        error: DriverError::Timeout,
                        delivery,
                    };
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return ExecOutcome::Failed {
                        error: DriverError::Platform("worker lost"),
                        delivery: Delivery::DeliveredUnverified,
                    };
                }
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl<A: Actuator + 'static> fmt::Debug for Driver<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Driver(observation: {}, releases: {}, busy: {})",
            lock(&self.latest).is_some(),
            self.release_generation(),
            self.is_busy()
        )
    }
}

impl<A: Actuator + 'static> DesktopDriver for Driver<A> {
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome {
        let _one_at_a_time = lock(&self.observe_lock);
        // Whatever happens, the previous observation is no longer actionable.
        *lock(&self.latest) = None;
        match self.firewall.preflight() {
            SessionState::Active => {}
            SessionState::Paused(p) => return ObserveOutcome::Paused(p),
            SessionState::Blocked(b) => return ObserveOutcome::Blocked(b),
        }
        match self.firewall.observe(deny) {
            ObserveVerdict::Clear(o) => {
                let screen = ObservedScreen::from_observation(&o);
                *lock(&self.latest) = Some(Latest {
                    obs: Arc::new(o),
                    spent: false,
                    cached_at: self.clock.now(),
                });
                ObserveOutcome::Clear(screen)
            }
            ObserveVerdict::Withheld(w) => ObserveOutcome::Withheld(w),
            ObserveVerdict::Paused(p) => ObserveOutcome::Paused(p),
            ObserveVerdict::Blocked(b) => ObserveOutcome::Blocked(b),
        }
    }

    fn describe(&self, r: ElementRef) -> Result<TargetDescriptor, DriverError> {
        let obs = {
            let g = lock(&self.latest);
            match g.as_ref() {
                Some(l) if !l.spent => Arc::clone(&l.obs),
                _ => return Err(DriverError::StaleRef),
            }
        };
        if r.observation != obs.id() {
            return Err(DriverError::StaleRef);
        }
        if obs.is_restricted_app() {
            return Err(DriverError::RestrictedApp);
        }
        obs.describe(r).ok_or(DriverError::NoSemanticPath)
    }

    fn execute(
        &self,
        approved: ApprovedAction,
        deny: &dyn AppDenyPolicy,
        kill: &KillSwitch,
    ) -> ExecOutcome {
        let epoch0 = kill.epoch();
        // (a) The approval must be live: kill epoch, void generation and age. No firewall or OS
        // call has happened yet.
        if approved.check_live().is_err() {
            return ExecOutcome::Refused(DriverError::Revoked);
        }
        let now = self.clock.now();
        // (b) The kill switch moved since this execute began.
        if kill.is_tripped_since(epoch0) {
            return ExecOutcome::Refused(DriverError::Killed);
        }
        // (c) Rate limits.
        if !lock(&self.limiter).admit(now) {
            return ExecOutcome::Refused(DriverError::RateLimited);
        }
        // (d) The observation: the latest one, unspent, the one the action was bound to, fresh.
        // It is spent here, immediately: one observation = one execute, even on failure.
        let obs = {
            let mut g = lock(&self.latest);
            let Some(l) = g.as_mut() else {
                return ExecOutcome::Refused(DriverError::StaleRef);
            };
            if l.spent || l.obs.id() != approved.step().observation() {
                return ExecOutcome::Refused(DriverError::StaleRef);
            }
            l.spent = true;
            if now.saturating_sub(l.cached_at) > OBSERVATION_MAX_AGE
                || l.obs.age() > OBSERVATION_MAX_AGE
            {
                return ExecOutcome::Refused(DriverError::StaleRef);
            }
            Arc::clone(&l.obs)
        };
        // (e) + static per-op checks (no OS access).
        let op = match plan_op(&approved, &obs, &self.launch, deny) {
            Ok(op) => op,
            Err(e) => return ExecOutcome::Refused(e),
        };
        // The deny policy travels to the worker as a snapshot of every app id it can be asked
        // about; anything else counts as denied.
        let mut ids: Vec<&str> = vec![obs.app_id().as_str()];
        ids.extend(obs.apps().iter().map(AppId::as_str));
        if let Op::FocusApp(a) = &op {
            ids.push(a.as_str());
        }
        let snapshot = DenySnapshot::take(deny, ids);
        // (f)-(i) on one worker thread under a deadline.
        self.run_on_worker(approved, op, obs, snapshot, kill, epoch0)
    }

    fn emergency_release(&self) {
        // Bump first: any primitive that is past its gate releases itself when it returns, none
        // starts.
        self.release_gen.fetch_add(1, Ordering::SeqCst);
        let _ = catch_unwind(AssertUnwindSafe(|| self.actuator.release_all()));
    }
}

/// Step (e) and the static part of (f): descriptor equality, built-in refusals, role/key/launch
/// rules. Pure: no OS access, no actuator.
fn plan_op(
    approved: &ApprovedAction,
    obs: &Observation,
    launch: &LaunchAllowlist,
    deny: &dyn AppDenyPolicy,
) -> Result<Op, DriverError> {
    // Redundant defence: the gate never mints for a restricted app and `Firewall::revalidate`
    // refuses it again; `describe` already hides its descriptors. Not reachable through the real
    // gate, so it has no test of its own.
    if obs.is_restricted_app() {
        return Err(DriverError::RestrictedApp);
    }
    let act = approved.action();
    // (e) ApprovedAction descriptor == obs.describe(target), field by field.
    let desc = approved.step().descriptor();
    check_descriptor(act.target(), desc, obs)?;
    plan_kind(
        act.kind(),
        desc,
        obs.id(),
        obs.flags(),
        obs.apps(),
        launch,
        deny,
    )
}

/// Step (e): the descriptor the approval was bound to must equal the observation's own, field by
/// field. Pure: no OS access.
pub(crate) fn check_descriptor(
    act_target: Option<ElementRef>,
    desc: Option<&TargetDescriptor>,
    obs: &Observation,
) -> Result<(), DriverError> {
    match (act_target, desc) {
        (Some(t), Some(d)) => {
            if t.observation != obs.id() {
                return Err(DriverError::StaleRef);
            }
            let live = obs.describe(t).ok_or(DriverError::DescriptorMismatch)?;
            if !same_descriptor(d, &live) {
                return Err(DriverError::DescriptorMismatch);
            }
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(DriverError::DescriptorMismatch),
    }
}

pub(crate) fn same_descriptor(a: &TargetDescriptor, b: &TargetDescriptor) -> bool {
    a.element == b.element
        && a.app_id == b.app_id
        && a.role == b.role
        && a.label == b.label
        && a.label_sha256 == b.label_sha256
        && a.rect == b.rect
        && a.editable == b.editable
        && a.sensitive_label == b.sensitive_label
}

pub(crate) fn click_method(role: NodeRole) -> Option<ClickMethod> {
    match role {
        NodeRole::Button | NodeRole::Link | NodeRole::MenuItem => Some(ClickMethod::Invoke),
        NodeRole::CheckBox => Some(ClickMethod::Toggle),
        NodeRole::RadioButton | NodeRole::Tab | NodeRole::ListItem => Some(ClickMethod::Select),
        NodeRole::ComboBox => Some(ClickMethod::ExpandCollapse),
        _ => None,
    }
}

/// Static per-op rules (ADR 0004 sections 7, 8, 14).
pub(crate) fn plan_kind(
    kind: &ActionKind,
    desc: Option<&TargetDescriptor>,
    obs_id: firewall::ObservationId,
    flags: ObservationFlags,
    apps: &[AppId],
    launch: &LaunchAllowlist,
    deny: &dyn AppDenyPolicy,
) -> Result<Op, DriverError> {
    // Payment page: only Scroll and Focus may even be tried (the firewall refuses Focus there
    // too, which is stricter than the ADR text and kept on purpose).
    if flags.payment_suspected
        && !matches!(kind, ActionKind::Scroll { .. } | ActionKind::Focus { .. })
    {
        return Err(DriverError::PaymentSuspected);
    }
    let need = || desc.ok_or(DriverError::DescriptorMismatch);
    match kind {
        ActionKind::Focus { target } => {
            need()?;
            Ok(Op::Focus(*target))
        }
        ActionKind::Scroll {
            target,
            direction,
            amount,
            repeat,
        } => {
            let sel = match target {
                ScrollTarget::Element(e) => {
                    need()?;
                    TargetSel::Element(*e)
                }
                ScrollTarget::Window(id) => {
                    if *id != obs_id {
                        return Err(DriverError::StaleRef);
                    }
                    TargetSel::ScrollRoot
                }
            };
            Ok(Op::Scroll {
                sel,
                dir: *direction,
                amount: *amount,
                repeat: *repeat,
            })
        }
        ActionKind::Click { target } => {
            let d = need()?;
            let m = click_method(d.role).ok_or(DriverError::NoSemanticPath)?;
            Ok(Op::Click(*target, m))
        }
        ActionKind::SetText { target, text } | ActionKind::TypeText { target, text } => {
            let d = need()?;
            if d.sensitive_label {
                return Err(DriverError::SensitiveTarget);
            }
            if !d.editable {
                return Err(DriverError::NotEditable);
            }
            Ok(if matches!(kind, ActionKind::SetText { .. }) {
                Op::SetText(*target, text.clone())
            } else {
                Op::TypeText(*target, text.clone())
            })
        }
        ActionKind::KeyCombo { target, combo } => {
            let d = need()?;
            if d.sensitive_label {
                return Err(DriverError::SensitiveTarget);
            }
            if combo.needs_editable_target() && !d.editable {
                return Err(DriverError::KeyNotAllowedHere);
            }
            Ok(Op::Key(*target, *combo))
        }
        ActionKind::CopySelection { target } => {
            let d = need()?;
            if d.sensitive_label {
                return Err(DriverError::SensitiveTarget);
            }
            Ok(Op::Copy(*target))
        }
        ActionKind::OpenApp { app } => {
            let path = launch.resolve(app).ok_or(DriverError::LaunchNotAllowed)?;
            Ok(Op::Launch(path.to_path_buf()))
        }
        ActionKind::FocusApp { app } => {
            if !apps.contains(app) {
                return Err(DriverError::AppNotObserved);
            }
            if firewall::is_auth_surface_app(app.as_str()) {
                return Err(DriverError::AuthSurface);
            }
            if firewall::is_restricted_app(app.as_str()) {
                return Err(DriverError::RestrictedApp);
            }
            if deny.is_denied(app.as_str()) {
                return Err(DriverError::DeniedApp);
            }
            Ok(Op::FocusApp(app.clone()))
        }
    }
}
