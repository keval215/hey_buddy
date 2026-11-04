//! The kill controller (ADR 0003 section 9, S2-13): one `trip` stops everything, instantly and
//! independent of the engine or the driver answering.
//!
//! `trip` is non-blocking from the caller's view: it moves the kill epoch first (one lock-free
//! `fetch_add`, so every `ApprovedAction::check_live` fails from that instant), then hands the
//! rest to a dedicated worker thread (std thread: not tokio, not the Tauri main loop). The
//! worker runs these steps IN PARALLEL, each on its own short-lived thread with its own timeout
//! and panic isolation, so a hung or panicking collaborator never prevents the others:
//!
//! 1. engine kill (the sandboxed engine process / job; S2-10 implements it),
//! 2. runner abort (the plan runner task / cancel flag),
//! 3. the switch's hooks: the gate's `void_all(Killed)` (pending approvals void, plan digest
//!    retired, grants dead, mode Observe, audit `killed`, event `Killed`),
//! 4. driver emergency release (only the keys / buttons the driver itself pressed; needs no
//!    `ApprovedAction`).
//!
//! Then, after those, the driver is released a SECOND time (a primitive already past `check_live`
//! may have pressed a key after the first release), the app is verified to be in Observe mode (a second `void_all` if not) and
//! the buddy says "Stopped." through the TTS path on a detached thread (a hung speaker
//! never holds the worker). An in-flight OS call cannot be interrupted;
//! what is guaranteed is that nothing NEW is started (docs/kill-switch.md).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc as fmpsc;
use futures::{SinkExt, StreamExt};
use policy::{KillSwitch, Mode, PolicyConfig, PolicyGate, SystemClock, VoidReason};
use voice::TtsOptions;

pub use policy::KillReason;

use crate::activity::GateProbe;
use crate::buddy::Buddy;
use crate::spoken::{self, Locale, Notice};

// ---------------------------------------------------------------------------------------------
// Collaborators (all with fakes)
// ---------------------------------------------------------------------------------------------

/// Kills the engine process / job. MUST be safe to call from any thread at any time; the
/// controller never waits for it beyond its timeout.
pub trait EngineKiller: Send + Sync {
    fn kill_engine(&self);
}

/// Releases keys and buttons the driver itself pressed. Needs NO `ApprovedAction`.
pub trait DriverReleaser: Send + Sync {
    fn emergency_release(&self);
}

/// Says the fixed sentence. Must not be required for the kill to take effect.
pub trait Speaker: Send + Sync {
    fn say_stopped(&self);
}

/// The plan runner: abort its task / set its cancel flag.
pub trait RunnerHandle: Send + Sync {
    fn abort(&self);
}

/// No engine is running.
pub struct NoEngine;
impl EngineKiller for NoEngine {
    fn kill_engine(&self) {}
}
/// No driver is attached.
pub struct NoDriver;
impl DriverReleaser for NoDriver {
    fn emergency_release(&self) {}
}
/// Silent.
pub struct NoSpeaker;
impl Speaker for NoSpeaker {
    fn say_stopped(&self) {}
}
/// No runner is attached.
pub struct NoRunner;
impl RunnerHandle for NoRunner {
    fn abort(&self) {}
}

/// Records calls (tests). Optionally blocks forever (`hang`) or panics.
#[derive(Default)]
pub struct FakeEngineKiller {
    pub calls: AtomicU64,
    pub hang: std::sync::atomic::AtomicBool,
    pub boom: std::sync::atomic::AtomicBool,
}
impl EngineKiller for FakeEngineKiller {
    fn kill_engine(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.boom.load(Ordering::SeqCst) {
            panic!("engine killer exploded");
        }
        if self.hang.load(Ordering::SeqCst) {
            loop {
                thread::park();
            }
        }
    }
}

/// Records calls (tests).
#[derive(Default)]
pub struct FakeDriverReleaser {
    pub calls: AtomicU64,
    pub boom: std::sync::atomic::AtomicBool,
}
impl DriverReleaser for FakeDriverReleaser {
    fn emergency_release(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.boom.load(Ordering::SeqCst) {
            panic!("releaser exploded");
        }
    }
}

/// Records calls (tests). Optionally blocks forever.
#[derive(Default)]
pub struct FakeSpeaker {
    pub calls: AtomicU64,
    pub hang: std::sync::atomic::AtomicBool,
}
impl Speaker for FakeSpeaker {
    fn say_stopped(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang.load(Ordering::SeqCst) {
            loop {
                thread::park();
            }
        }
    }
}

/// Records calls (tests).
#[derive(Default)]
pub struct FakeRunnerHandle {
    pub calls: AtomicU64,
}
impl RunnerHandle for FakeRunnerHandle {
    fn abort(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Says "Stopped." through the buddy's TTS engine and audio output (the same path as the other
/// fixed sentences). Silent when no voice engine is attached.
pub struct BuddySpeaker {
    buddy: Arc<Buddy>,
}

impl BuddySpeaker {
    pub fn new(buddy: Arc<Buddy>) -> Self {
        Self { buddy }
    }
}

impl Speaker for BuddySpeaker {
    fn say_stopped(&self) {
        let Some(tts) = self.buddy.tts_slot().get() else {
            return;
        };
        let Some(deps) = self.buddy.engine_deps() else {
            return;
        };
        let text = spoken::notice(Notice::Stopped, Locale::En).to_string();
        let handle = deps.runtime.clone();
        handle.block_on(async move {
            let (mut tx, rx) = fmpsc::unbounded::<String>();
            let Ok(audio) = tts.synthesize(rx.boxed(), TtsOptions::default()).await else {
                return;
            };
            let Ok(playback) = deps.audio_out.play(audio) else {
                return;
            };
            let _ = tx.send(text).await;
            drop(tx);
            playback.finished().await;
        });
    }
}

// ---------------------------------------------------------------------------------------------
// The controller
// ---------------------------------------------------------------------------------------------

/// How one step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Done,
    Panicked,
    TimedOut,
    /// The helper thread could not be started.
    NotStarted,
}

/// What a kill run did, per step (fixed tokens only). For tests, logs and the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillReport {
    pub reason: KillReason,
    pub engine: StepOutcome,
    pub runner: StepOutcome,
    pub gate: StepOutcome,
    pub driver: StepOutcome,
    pub speaker: StepOutcome,
    /// The gate reported Observe mode after the kill.
    pub observe: bool,
}

/// Per-step time limits.
#[derive(Debug, Clone, Copy)]
pub struct KillTimeouts {
    /// Engine kill, runner abort, gate void, driver release (run in parallel).
    pub step: Duration,
    /// "Stopped." (after the others).
    pub speak: Duration,
}

impl Default for KillTimeouts {
    fn default() -> Self {
        Self {
            step: Duration::from_secs(3),
            speak: Duration::from_secs(5),
        }
    }
}

struct Parts {
    engine: Arc<dyn EngineKiller>,
    driver: Arc<dyn DriverReleaser>,
    speaker: Arc<dyn Speaker>,
    runner: Arc<dyn RunnerHandle>,
}

struct Shared {
    gate: Arc<PolicyGate>,
    kill: KillSwitch,
    timeouts: KillTimeouts,
    parts: Mutex<Parts>,
    started: AtomicU64,
    finished: Mutex<u64>,
    cv: Condvar,
    last: Mutex<Option<KillReport>>,
}

pub struct KillController {
    shared: Arc<Shared>,
    tx: Mutex<Option<Sender<KillReason>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl KillController {
    /// Builds the controller for `gate` (it owns a clone of the gate's kill switch) with no
    /// collaborators; install them with the `set_*` methods.
    pub fn new(gate: Arc<PolicyGate>) -> Arc<Self> {
        Self::with_timeouts(gate, KillTimeouts::default())
    }

    /// The app's controller: a gate over the buddy's activity log (until the agent is wired in,
    /// S2-23, the gate only serves the kill path and the activity probe), the real speaker,
    /// and the activity probe installed on the buddy.
    pub fn for_buddy(buddy: &Arc<Buddy>) -> Arc<Self> {
        let gate = PolicyGate::new(
            PolicyConfig::default(),
            KillSwitch::new(),
            buddy.audit_sink(),
            Arc::new(SystemClock::new()),
        );
        buddy.set_activity_probe(Arc::new(GateProbe(Arc::clone(&gate))));
        let ctl = Self::new(gate);
        ctl.set_speaker(Arc::new(BuddySpeaker::new(Arc::clone(buddy))));
        ctl
    }

    /// The gate this controller stops.
    pub fn gate(&self) -> &Arc<PolicyGate> {
        &self.shared.gate
    }

    pub fn with_timeouts(gate: Arc<PolicyGate>, timeouts: KillTimeouts) -> Arc<Self> {
        let shared = Arc::new(Shared {
            kill: gate.kill_switch().clone(),
            gate,
            timeouts,
            parts: Mutex::new(Parts {
                engine: Arc::new(NoEngine),
                driver: Arc::new(NoDriver),
                speaker: Arc::new(NoSpeaker),
                runner: Arc::new(NoRunner),
            }),
            started: AtomicU64::new(0),
            finished: Mutex::new(0),
            cv: Condvar::new(),
            last: Mutex::new(None),
        });
        let (tx, rx) = channel::<KillReason>();
        let worker = Arc::clone(&shared);
        let tx = thread::Builder::new()
            .name("kill-worker".into())
            .spawn(move || worker_loop(&worker, &rx))
            .ok()
            .map(|_| tx);
        Arc::new(Self {
            shared,
            tx: Mutex::new(tx),
        })
    }

    pub fn set_engine_killer(&self, k: Arc<dyn EngineKiller>) {
        lock(&self.shared.parts).engine = k;
    }
    pub fn set_driver_releaser(&self, d: Arc<dyn DriverReleaser>) {
        lock(&self.shared.parts).driver = d;
    }
    pub fn set_speaker(&self, s: Arc<dyn Speaker>) {
        lock(&self.shared.parts).speaker = s;
    }
    pub fn set_runner(&self, r: Arc<dyn RunnerHandle>) {
        lock(&self.shared.parts).runner = r;
    }

    /// STOP. Returns the new kill epoch. The epoch moves FIRST and lock-free; everything else
    /// is handed to the worker, so this never waits for the gate, the engine, the driver or the
    /// speaker. Idempotent: tripping again just stops again. Harmless when nothing runs.
    pub fn trip(&self, reason: KillReason) -> u64 {
        let epoch = self.shared.kill.bump();
        self.shared.started.fetch_add(1, Ordering::SeqCst);
        let sent = lock(&self.tx)
            .as_ref()
            .is_some_and(|tx| tx.send(reason).is_ok());
        if !sent {
            // No worker (it could not start, or died): do the minimum on a fresh thread; as a
            // last resort run the hooks inline. The epoch has already moved either way.
            let shared = Arc::clone(&self.shared);
            let spawned = thread::Builder::new()
                .name("kill-fallback".into())
                .spawn(move || run_sequence(&shared, reason, 1))
                .is_ok();
            if !spawned {
                self.shared.kill.run_hooks(reason);
                finish(&self.shared, 1);
            }
        }
        epoch
    }

    /// Blocks until every trip so far has finished its sequence (tests, shutdown). `false` on
    /// timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut done = lock(&self.shared.finished);
        loop {
            if *done >= self.shared.started.load(Ordering::SeqCst) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            done = self
                .shared
                .cv
                .wait_timeout(done, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The report of the most recent finished kill.
    pub fn last_report(&self) -> Option<KillReport> {
        *lock(&self.shared.last)
    }

    /// The runaway / limit detector's entry point (called by the supervisor later).
    pub fn trip_limit(&self) -> u64 {
        self.trip(KillReason::Limit)
    }
}

fn worker_loop(shared: &Arc<Shared>, rx: &Receiver<KillReason>) {
    while let Ok(reason) = rx.recv() {
        // Coalesce trips that queued up meanwhile: one sequence stops everything.
        let mut n = 1u64;
        let mut last = reason;
        while let Ok(r) = rx.try_recv() {
            n += 1;
            last = r;
        }
        run_sequence(shared, last, n);
    }
}

fn finish(shared: &Shared, n: u64) {
    *lock(&shared.finished) += n;
    shared.cv.notify_all();
}

/// A started helper: its result arrives on `rx` before `deadline`.
struct Started {
    rx: Receiver<bool>,
    deadline: Instant,
    ok: bool,
}

fn start(timeout: Duration, f: impl FnOnce() + Send + 'static) -> Started {
    let (tx, rx) = channel::<bool>();
    let ok = thread::Builder::new()
        .name("kill-step".into())
        .spawn(move || {
            let ok = catch_unwind(AssertUnwindSafe(f)).is_ok();
            let _ = tx.send(ok);
        })
        .is_ok();
    Started {
        rx,
        deadline: Instant::now() + timeout,
        ok,
    }
}

fn wait(s: Started) -> StepOutcome {
    if !s.ok {
        return StepOutcome::NotStarted;
    }
    let left = s.deadline.saturating_duration_since(Instant::now());
    match s.rx.recv_timeout(left) {
        Ok(true) => StepOutcome::Done,
        Ok(false) | Err(RecvTimeoutError::Disconnected) => StepOutcome::Panicked,
        Err(RecvTimeoutError::Timeout) => StepOutcome::TimedOut,
    }
}

fn run_sequence(shared: &Arc<Shared>, reason: KillReason, n: u64) {
    let (engine, driver, speaker, runner) = {
        let p = lock(&shared.parts);
        (
            Arc::clone(&p.engine),
            Arc::clone(&p.driver),
            Arc::clone(&p.speaker),
            Arc::clone(&p.runner),
        )
    };
    let t = shared.timeouts;
    let kill = shared.kill.clone();
    // Start everything first (a hung one must not delay the others), then collect.
    let e = start(t.step, move || engine.kill_engine());
    let r = start(t.step, move || runner.abort());
    let g = start(t.step, move || kill.run_hooks(reason));
    let driver2 = Arc::clone(&driver);
    let d = start(t.step, move || driver.emergency_release());
    let (engine_o, runner_o, gate_o, driver_o) = (wait(e), wait(r), wait(g), wait(d));

    // A primitive already past `check_live` may have pressed a key after the first release:
    // release again now that the runner and engine steps are over (the releaser is idempotent).
    let _ = wait(start(t.step, move || driver2.emergency_release()));

    // Back to Observe, whatever happened above (idempotent second void if the hook did not run).
    // Reading the mode takes the gate lock, so it is a timed step too; unknown counts as not
    // observing.
    let observed = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&shared.gate);
    let seen = Arc::clone(&observed);
    let hook_ok = gate_o == StepOutcome::Done;
    let mode_o = wait(start(t.step, move || {
        let mut ok = gate.mode() == Mode::Observe;
        if !ok || !hook_ok {
            gate.void_all(VoidReason::Killed(reason));
            ok = gate.mode() == Mode::Observe;
        }
        seen.store(ok, Ordering::SeqCst);
    }));
    let observe = mode_o == StepOutcome::Done && observed.load(Ordering::SeqCst);

    // "Stopped." runs on its own detached thread, so a hung speaker can never hold the worker:
    // a second trip starts its hooks and engine kill at once. The thread publishes the report
    // and marks the trip finished when the speaker is done or its limit passes.
    let report = KillReport {
        reason,
        engine: engine_o,
        runner: runner_o,
        gate: gate_o,
        driver: driver_o,
        speaker: StepOutcome::NotStarted,
        observe,
    };
    let sh = Arc::clone(shared);
    let limit = t.speak;
    let spawned = thread::Builder::new()
        .name("kill-speak".into())
        .spawn(move || {
            let speaker_o = wait(start(limit, move || speaker.say_stopped()));
            *lock(&sh.last) = Some(KillReport {
                speaker: speaker_o,
                ..report
            });
            finish(&sh, n);
        })
        .is_ok();
    if !spawned {
        *lock(&shared.last) = Some(report);
        finish(shared, n);
    }
}
