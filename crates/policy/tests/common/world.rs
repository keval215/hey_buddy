//! A scripted desktop and a rig around a `PolicyGate` with a manual clock, shared by the
//! integration tests and (through `#[path]`) the crate's own unit tests. Uses only public API and
//! the `firewall` fakes (test-only).
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use firewall::fake::{ElemSpec, ScriptedDesktop, WindowSpec};
use firewall::{Firewall, NodeRole, Observation, ObserveVerdict, PrivacyFirewall, SessionState};
use frame::{AppDenyPolicy, Rect};
use policy::{
    bind_step, AuditSink, BindContext, BoundStep, Decision, DryRunReport, KillSwitch, ManualClock,
    MemoryAudit, MemoryEvents, Mode, PlanContext, PlanGrant, PolicyConfig, PolicyGate,
    ProposedPlan, StaticCatalogue, StepGrant, TaskId, TaskOrigin,
};

pub const TRANSCRIPT: &str =
    "open Notepad then type John Smith into the Name field and scroll down and save";
pub const OWN_PID: u32 = 1;
pub const HWND: u64 = 0x100;
pub const WIN: Rect = Rect::new(100, 100, 800, 600);

pub fn catalogue() -> StaticCatalogue {
    StaticCatalogue::new(&[
        ("Notepad", "notepad", "notepad.exe"),
        ("Calculator", "calc", "calculator.exe"),
        ("Command Prompt", "cmd", "cmd.exe"),
    ])
    .expect("valid catalogue")
}

struct DenyNone;
impl AppDenyPolicy for DenyNone {
    fn is_denied(&self, _app_id: &str) -> bool {
        false
    }
}

/// Elements (runtime id: role, label): 1 text field "Name" (editable), 2 password field,
/// 3 button "Save" (submit lexicon), 4 button "Refresh", 6 button "Delete",
/// 7 and 8 buttons "Twin" (ambiguous), 9 text field "Notes" (editable). Observation node numbers
/// (`#n`): Name 1, Password 2 (secure), Save 3, Refresh 4, Delete 5, Twin 6 and 7, Notes 8.
/// (Tabs are dropped by the firewall as browser chrome, so a Reversible click targets "Notes".)
pub fn window(app: &str) -> WindowSpec {
    window_with(HWND, 4242, app)
}

pub fn window_with(hwnd: u64, pid: u32, app: &str) -> WindowSpec {
    WindowSpec::new(hwnd, app, pid, WIN)
        .element(
            ElemSpec::new(1, NodeRole::TextField, "Name", Rect::new(200, 200, 300, 30))
                .editable(true),
        )
        .element(
            ElemSpec::new(
                2,
                NodeRole::TextField,
                "Password",
                Rect::new(200, 260, 300, 30),
            )
            .password(),
        )
        .element(ElemSpec::new(
            3,
            NodeRole::Button,
            "Save",
            Rect::new(200, 320, 100, 30),
        ))
        .element(ElemSpec::new(
            4,
            NodeRole::Button,
            "Refresh",
            Rect::new(320, 320, 100, 30),
        ))
        .element(ElemSpec::new(
            6,
            NodeRole::Button,
            "Delete",
            Rect::new(200, 400, 100, 30),
        ))
        .element(ElemSpec::new(
            7,
            NodeRole::Button,
            "Twin",
            Rect::new(200, 440, 100, 30),
        ))
        .element(ElemSpec::new(
            8,
            NodeRole::Button,
            "Twin",
            Rect::new(320, 440, 100, 30),
        ))
        .element(
            ElemSpec::new(
                9,
                NodeRole::TextField,
                "Notes",
                Rect::new(200, 500, 300, 30),
            )
            .editable(true),
        )
}

pub fn desktop() -> ScriptedDesktop {
    ScriptedDesktop::new().with_window(window("notes.exe"))
}

pub fn firewall_of(
    d: &ScriptedDesktop,
) -> PrivacyFirewall<
    ScriptedDesktop,
    ScriptedDesktop,
    ScriptedDesktop,
    ScriptedDesktop,
    ScriptedDesktop,
> {
    PrivacyFirewall::new(d.clone(), d.clone())
        .with_text_source(d.clone())
        .with_observation(d.clone(), d.clone())
        .with_own_pid(OWN_PID)
}

pub fn observe_of(d: &ScriptedDesktop) -> Observation {
    let f = firewall_of(d);
    assert_eq!(f.preflight(), SessionState::Active);
    match f.observe(&DenyNone) {
        ObserveVerdict::Clear(o) => o,
        other => panic!("expected Clear, got {other:?}"),
    }
}

pub fn steps_json(steps: &[&str]) -> String {
    format!(r#"{{"steps":[{}]}}"#, steps.join(","))
}

pub fn click(label: &str) -> String {
    format!(r#"{{"op":"click","target":{{"label":"{label}"}}}}"#)
}

pub fn type_into(label: &str, text: &str) -> String {
    format!(r#"{{"op":"type","target":{{"label":"{label}"}},"text":"{text}"}}"#)
}

pub const OBSERVE: &str = r#"{"op":"observe"}"#;
pub const SCROLL: &str = r#"{"op":"scroll","direction":"down","amount":3}"#;
pub const OPEN_NOTEPAD: &str = r#"{"op":"open_app","app":"Notepad"}"#;

/// Gate + manual clock + memory sinks + scripted desktop.
pub struct Rig {
    pub gate: Arc<PolicyGate>,
    pub clock: Arc<ManualClock>,
    pub audit: Arc<MemoryAudit>,
    pub events: Arc<MemoryEvents>,
    pub kill: KillSwitch,
    pub desk: ScriptedDesktop,
    next_task: std::sync::atomic::AtomicU64,
}

impl Rig {
    pub fn new() -> Self {
        Self::with_origin(TaskOrigin::Interactive)
    }

    pub fn with_origin(origin: TaskOrigin) -> Self {
        Self::with_config(PolicyConfig {
            origin,
            ..PolicyConfig::default()
        })
    }

    pub fn with_config(cfg: PolicyConfig) -> Self {
        Self::with_sink(cfg, |m| m)
    }

    /// As `with_config`, with the audit sink the gate writes to wrapped by `wrap` (the
    /// `MemoryAudit` in `rig.audit` stays the recorder behind it).
    pub fn with_sink(
        cfg: PolicyConfig,
        wrap: impl FnOnce(Arc<MemoryAudit>) -> Arc<dyn AuditSink>,
    ) -> Self {
        let clock = Arc::new(ManualClock::new());
        // Time must not start at zero so "just now" arithmetic is meaningful.
        clock.advance(Duration::from_secs(1000));
        let audit = Arc::new(MemoryAudit::new());
        let events = Arc::new(MemoryEvents::new());
        let kill = KillSwitch::new();
        let gate = PolicyGate::new(cfg, kill.clone(), wrap(audit.clone()), clock.clone());
        gate.set_event_sink(events.clone());
        Self {
            gate,
            clock,
            audit,
            events,
            kill,
            desk: desktop(),
            next_task: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.clock.advance(Duration::from_millis(ms));
    }

    pub fn propose(&self, draft: &str) -> ProposedPlan {
        self.try_propose(draft)
            .unwrap_or_else(|e| panic!("propose failed: {e}"))
    }

    pub fn try_propose(&self, draft: &str) -> Result<ProposedPlan, policy::PlanError> {
        let cat = catalogue();
        let id = self
            .next_task
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.gate
            .propose(TaskId(id), draft, &PlanContext::new(TRANSCRIPT, &cat))
    }

    pub fn ctx(&self, obs: &Observation, idx: u16) -> BindContext {
        let mut c = BindContext::new(
            self.gate.policy_context().clone(),
            SessionState::Active,
            obs.id(),
            self.gate.clock().now(),
        );
        c.step_index = idx;
        c.higher_integrity = Some(false);
        c.focused_secure = Some(false);
        c.target_denied = Some(false);
        c
    }

    pub fn observe(&self) -> Observation {
        observe_of(&self.desk)
    }

    pub fn dry(&self, plan: &ProposedPlan) -> DryRunReport {
        let obs = self.observe();
        self.gate.dry_run(plan, &obs, &self.ctx(&obs, 0))
    }

    /// Dry run, plan approval (after the input guard), Live grant.
    pub fn go_live(&self, draft: &str) -> (ProposedPlan, PlanGrant) {
        let plan = self.propose(draft);
        let grant = self.approve(&plan);
        (plan, grant)
    }

    pub fn approve(&self, plan: &ProposedPlan) -> PlanGrant {
        let report = self.dry(plan);
        self.approve_with(plan, &report)
    }

    pub fn approve_with(&self, plan: &ProposedPlan, report: &DryRunReport) -> PlanGrant {
        let pending = self
            .gate
            .request_plan_approval(plan, report)
            .expect("plan approval requested");
        self.advance(600);
        self.gate
            .decide(pending.id(), &pending.digest_hex(), Decision::Approve)
            .expect("decide");
        let grant = pending.wait().expect("plan grant");
        assert_eq!(self.gate.mode(), Mode::Live);
        grant
    }

    /// Fresh observation, noted, step `idx` of `plan` bound to it.
    pub fn bind(&self, plan: &ProposedPlan, idx: usize) -> BoundStep {
        let obs = self.observe();
        self.bind_in(plan, idx, &obs)
    }

    pub fn bind_in(&self, plan: &ProposedPlan, idx: usize, obs: &Observation) -> BoundStep {
        self.gate
            .note_observation(obs.id(), SessionState::Active)
            .expect("note observation");
        bind_step(&plan.steps()[idx], obs, &self.ctx(obs, idx as u16)).expect("bind")
    }
}

impl Rig {
    /// Step approval for `step`: request, wait out the input guard, approve, wait.
    pub fn step_grant(&self, step: &BoundStep) -> StepGrant {
        let pending = self
            .gate
            .request_step_approval(step)
            .expect("step approval requested");
        self.advance(600);
        self.gate
            .decide(pending.id(), &pending.digest_hex(), Decision::Approve)
            .expect("decide");
        pending.wait().expect("step grant")
    }

    /// Runs step `i` of `plan` the way the sequential runner does and returns what happened:
    /// read ops complete, everything else is evaluated, approved if asked, minted, executed.
    pub fn run_step(&self, plan: &ProposedPlan, grant: &mut PlanGrant, i: usize) {
        self.advance(300);
        let s = self.bind(plan, i);
        if s.action().is_none() {
            self.gate.complete_read(&s, grant).expect("read completes");
            return;
        }
        let single = match self.gate.evaluate(&s, Some(grant)) {
            policy::Evaluation::NeedsApproval => Some(self.step_grant(&s)),
            policy::Evaluation::Allow => None,
            other => panic!("unexpected verdict {other:?}"),
        };
        let a = self.gate.authorize(s, grant, single).expect("mint");
        a.check_live().expect("live");
        self.gate
            .record_result(a.digest(), &policy::ExecResult::executed());
    }
}

impl Default for Rig {
    fn default() -> Self {
        Self::new()
    }
}
