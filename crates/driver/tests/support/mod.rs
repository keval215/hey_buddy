//! Shared rig for the driver tests: the policy crate's test world (REAL `PolicyGate`, manual
//! clock, `ScriptedDesktop`), the REAL `PrivacyFirewall` and the REAL `Driver` over a
//! `FakeActuator`. There is no fake firewall and no fake gate.
#![allow(dead_code)]

#[path = "../../../policy/tests/common/world.rs"]
pub mod world;

use std::sync::Arc;
use std::time::Duration;

use driver::fake::FakeActuator;
use driver::{DesktopDriver, Driver, LaunchAllowlist, ObserveOutcome, ObservedScreen};
use firewall::fake::{ElemSpec, ScriptedDesktop, WindowSpec};
use firewall::{ElementRef, NodeRole, Observation};
use frame::{AppDenyPolicy, Rect};
use policy::{
    ApprovedAction, BindContext, Evaluation, ManualClock, PlanGrant, ProposedPlan, StepGrant,
};
pub use world::*;

/// Alias so no `ApprovedAction` is followed by `{` (the CI tripwire in ci.yml).
pub type Act = ApprovedAction;

pub struct DenyNone;
impl AppDenyPolicy for DenyNone {
    fn is_denied(&self, _a: &str) -> bool {
        false
    }
}

pub struct DenyApp(pub &'static str);
impl AppDenyPolicy for DenyApp {
    fn is_denied(&self, a: &str) -> bool {
        a.eq_ignore_ascii_case(self.0)
    }
}

pub type Drv = Driver<FakeActuator>;

pub struct Env {
    pub rig: Rig,
    pub drv: Arc<Drv>,
    pub fake: FakeActuator,
    /// The driver's clock (the gate has its own, `rig.clock`).
    pub dclock: Arc<ManualClock>,
    /// Advance the driver clock 300 ms before each `exec` (off for the rate-limit tests).
    pub auto_space: std::sync::atomic::AtomicBool,
    /// What the user said (apps named in a plan must appear in it).
    pub transcript: std::sync::Mutex<String>,
    next_task: std::sync::atomic::AtomicU64,
}

pub const NOTEPAD_EXE: &str = "notepad.exe";
pub const LONG_TEXT: &str = "open Notepad then type John Smith into the Name field and scroll";

/// Standard window plus a checkbox (runtime id 10) and a combo box (11), plus a second app.
pub fn rich_desktop() -> ScriptedDesktop {
    let win = window("notes.exe")
        .element(ElemSpec::new(
            10,
            NodeRole::CheckBox,
            "Remember",
            Rect::new(200, 540, 100, 30),
        ))
        .element(ElemSpec::new(
            11,
            NodeRole::ComboBox,
            "Size",
            Rect::new(320, 540, 100, 30),
        ));
    let d = ScriptedDesktop::new().with_window(win);
    d.add_window(WindowSpec::new(0x200, NOTEPAD_EXE, 5151, WIN));
    d
}

impl Env {
    pub fn new() -> Self {
        Self::with_desk(rich_desktop())
    }

    pub fn with_desk(desk: ScriptedDesktop) -> Self {
        Self::build(desk, |c| c)
    }

    /// `wrap` may replace the driver's clock (it receives the manual clock the env keeps).
    pub fn build(
        desk: ScriptedDesktop,
        wrap: impl FnOnce(Arc<ManualClock>) -> Arc<dyn policy::Clock>,
    ) -> Self {
        let mut rig = Rig::new();
        rig.desk = desk;
        let fw = Arc::new(firewall_of(&rig.desk));
        let fake = FakeActuator::new(rig.desk.clone());
        let dclock = Arc::new(ManualClock::new());
        dclock.advance(Duration::from_secs(1000));
        let used_clock = wrap(dclock.clone());
        let launch = LaunchAllowlist::empty()
            .with("notepad", "C:\\Windows\\System32\\notepad.exe")
            .expect("allowlist");
        let drv = Driver::new(fw, fake.clone())
            .with_clock(used_clock)
            .with_launch_allowlist(launch)
            .with_call_deadline(Duration::from_millis(600));
        Self {
            rig,
            drv: Arc::new(drv),
            fake,
            dclock,
            auto_space: std::sync::atomic::AtomicBool::new(true),
            transcript: std::sync::Mutex::new(format!("{TRANSCRIPT} and open Calculator")),
            next_task: std::sync::atomic::AtomicU64::new(5000),
        }
    }

    pub fn observe(&self) -> ObservedScreen {
        match self.drv.observe(&DenyNone) {
            ObserveOutcome::Clear(s) => s,
            other => panic!("expected Clear, got {other:?}"),
        }
    }

    /// `#node` of the current driver observation.
    pub fn el(&self, node: u32) -> ElementRef {
        self.drv
            .with_latest(|o| o.element(node))
            .expect("an observation")
            .expect("addressable node")
    }

    /// Execute and report the result to the gate exactly once, like core does.
    pub fn exec(&self, a: Act) -> driver::ExecOutcome {
        if self.auto_space.load(std::sync::atomic::Ordering::SeqCst) {
            self.dclock.advance(Duration::from_millis(300));
        }
        let digest = a.digest();
        let out = self.drv.execute(a, &DenyNone, &self.rig.kill);
        self.rig.gate.record_result(digest, &out.exec_result());
        out
    }

    /// Full gate flow for a one-step plan: propose, dry run, plan approval, a FRESH driver
    /// observation, bind, evaluate (step approval when asked), authorize.
    pub fn approved(&self, step: &str) -> Act {
        self.approved_with(step, |_, _| {})
            .unwrap_or_else(|e| panic!("mint: {e}"))
    }

    pub fn approved_with(
        &self,
        step: &str,
        tweak: impl FnOnce(&mut BindContext, &Observation),
    ) -> Result<Act, String> {
        let plan = self.propose(&[step])?;
        let mut grant: PlanGrant = self.rig.approve(&plan);
        self.rig.advance(300);
        self.observe();
        let bound = self
            .drv
            .with_latest(|o| -> Result<_, String> {
                self.rig
                    .gate
                    .note_observation(o.id(), firewall::SessionState::Active)
                    .map_err(|e| format!("note: {e:?}"))?;
                let mut ctx = self.rig.ctx(o, 0);
                tweak(&mut ctx, o);
                policy::bind_step(&plan.steps()[0], o, &ctx).map_err(|e| format!("bind: {e}"))
            })
            .ok_or("no observation")??;
        let single: Option<StepGrant> = match self.rig.gate.evaluate(&bound, Some(&grant)) {
            Evaluation::NeedsApproval => Some(self.rig.step_grant(&bound)),
            Evaluation::Allow => None,
            other => return Err(format!("evaluate: {other:?}")),
        };
        self.rig
            .gate
            .authorize(bound, &mut grant, single)
            .map_err(|e| format!("authorize: {e:?}"))
    }

    pub fn propose(&self, steps: &[&str]) -> Result<ProposedPlan, String> {
        let cat = catalogue();
        let said = self.transcript.lock().unwrap().clone();
        let id = self
            .next_task
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.rig
            .gate
            .propose(
                policy::TaskId(id),
                &steps_json(steps),
                &policy::PlanContext::new(&said, &cat),
            )
            .map_err(|e| format!("propose: {e}"))
    }

    /// Mint with the focused element (for key combos / copy) set to `#node`.
    pub fn approved_focused(&self, step: &str, node: u32) -> Result<Act, String> {
        self.approved_with(step, move |ctx, o| {
            ctx.focus_target = o.element(node);
        })
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

/// A clock that trips a kill switch the first time it is read after `arm(kill)`.
pub struct TripClock {
    inner: Arc<ManualClock>,
    kill: std::sync::Mutex<Option<policy::KillSwitch>>,
}

impl TripClock {
    pub fn new(inner: Arc<ManualClock>) -> Self {
        Self {
            inner,
            kill: std::sync::Mutex::new(None),
        }
    }
    pub fn arm(&self, kill: policy::KillSwitch) {
        *self.kill.lock().unwrap() = Some(kill);
    }
}

impl policy::Clock for TripClock {
    fn now(&self) -> Duration {
        if let Some(k) = self.kill.lock().unwrap().take() {
            k.trip(policy::KillReason::Hotkey);
        }
        self.inner.now()
    }
}
