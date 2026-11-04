//! The agent supervisor (ADR 0003 section 1, S2-17 + S2-23): the single place where
//!
//! ```text
//! voice request -> plan (engine) -> policy::propose -> dry run -> approval (broker) -> gate plan grant
//!   -> per step: fresh observe -> bind -> gate.evaluate -> [step approval] -> gate.authorize
//!   -> driver.execute(ApprovedAction) -> gate.record_result -> audit
//! ```
//!
//! is wired, with the kill switch live between every phase. Everything is generic over traits, so
//! the whole flow runs on Linux with fakes. See `docs/supervisor.md`.
//!
//! The supervisor never decides anything the gate decides: it asks `PolicyGate` for every verdict
//! and for the only minter of `ApprovedAction`. It adds, on top of the gate (never instead of it):
//! the frozen-plan check (`outside_plan`), the dry-run-then-live repeat check, runaway / time /
//! step limits, one task at a time, and the wiring of the kill switch to the engine, the driver
//! and the runner. Whatever it is unsure about it stops. The engine is called ONCE per task: tool
//! results, observations and screen text never go back to the planner (no replanning in v1).
//! `Debug` and audit records carry fixed tokens and numbers only.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::{PlanInput, PlanLimits, PlanOutcome, SystemContext};
use firewall::SessionState;
use frame::AppDenyPolicy;
use policy::{
    AuditRecord, AuditSink, BindContext, BoundStep, Catalogue, Clock, Decision, DryRunReport,
    DryRunStep, Evaluation, ExecResult, GateError, KillSwitch, PlanContext, PlanGrant, PlanStep,
    PolicyGate, ProposedPlan, Refusal, StepGrant, Tier, PLAN_APPROVAL_TIMEOUT,
    STEP_APPROVAL_TIMEOUT,
};

use super::port::{peek, AgentDriver, BindExtras, BindFacts, DriverRelease};
use super::read::{ReadAnswer, ReadKind, MAX_ANSWERS};
use super::types::{
    AgentNotice, AgentSettings, DenyProvider, EngineFactory, Notifier, Pace, PauseReason,
    RefuseReason, RunLimits, ScheduleSource, Stage, TaskOutcome, TaskReport, TaskRequest,
};
use crate::approval::{
    ApprovalBroker, ApprovalDigest, ApprovalOutcome, OpKind, PendingApproval, StepFacts,
};
use crate::gateway::ProcessEngineKiller;
use crate::kill::{KillController, RunnerHandle};

/// A refused step is retried never; this is the gap the supervisor leaves between two mints
/// (the gate's floor is 250 ms, the driver's too).
const MINT_GAP: Duration = Duration::from_millis(300);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why a supervisor could not be built. Fixed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorError {
    /// `SupervisorParts::audit` is not the gate's audit sink.
    AuditMismatch,
}

impl SupervisorError {
    pub fn code(self) -> &'static str {
        match self {
            SupervisorError::AuditMismatch => "audit_mismatch",
        }
    }
}

/// Everything the supervisor owns or borrows. `controller` supplies the ONE gate (the supervisor
/// never builds a second one) and the kill switch.
pub struct SupervisorParts {
    pub controller: Arc<KillController>,
    /// The sink the gate was built with (`buddy.audit_sink()`): supervisor records go to the same
    /// append-only log. `AgentSupervisor::new` checks it is the gate's own (`audit_mismatch`).
    pub audit: Arc<dyn AuditSink>,
    pub broker: ApprovalBroker,
    pub driver: Arc<dyn AgentDriver>,
    pub factory: Arc<dyn EngineFactory>,
    pub settings: Arc<dyn AgentSettings>,
    pub deny: Arc<dyn DenyProvider>,
    pub catalogue: Arc<dyn Catalogue + Send + Sync>,
    /// Facts the gate needs that an observation cannot tell. `UnknownFacts` fails closed.
    pub facts: Arc<dyn BindFacts>,
    pub notifier: Arc<dyn Notifier>,
    pub pace: Arc<dyn Pace>,
    pub interactive_limits: RunLimits,
    pub unattended_limits: RunLimits,
}

impl SupervisorParts {
    /// Defaults: unknown facts (fail closed), no notifier, real-time pacing, standard limits.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        controller: Arc<KillController>,
        audit: Arc<dyn AuditSink>,
        broker: ApprovalBroker,
        driver: Arc<dyn AgentDriver>,
        factory: Arc<dyn EngineFactory>,
        settings: Arc<dyn AgentSettings>,
        deny: Arc<dyn DenyProvider>,
        catalogue: Arc<dyn Catalogue + Send + Sync>,
    ) -> Self {
        Self {
            controller,
            audit,
            broker,
            driver,
            factory,
            settings,
            deny,
            catalogue,
            facts: Arc::new(super::port::UnknownFacts),
            notifier: Arc::new(super::types::NoNotifier),
            pace: Arc::new(super::types::TokioPace),
            interactive_limits: RunLimits::interactive(),
            unattended_limits: RunLimits::unattended(),
        }
    }
}

/// Test hooks (compiled only into this crate's unit tests).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestHooks {
    /// Replace the step run at this index with another validated step (an injected step).
    pub step_override: Mutex<Option<(usize, PlanStep)>>,
    /// The digest asked of the broker differs from the gate's.
    pub digest_tamper: AtomicBool,
    /// Skip the fresh observation before this step index (an observation reuse attempt).
    pub reuse_observation_at: Mutex<Option<usize>>,
    /// Runs at one of the kill-check points (a kill landing exactly in that window).
    pub at: Mutex<Option<HookAt>>,
}

/// A hook and where it runs.
#[cfg(test)]
pub(crate) type HookAt = (HookPoint, Arc<dyn Fn() + Send + Sync>);

/// The windows between phases where the kill switch is checked (one hook point each).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookPoint {
    /// After `attach` and the foreground read, right before `engine.plan`.
    BeforePlan,
    /// After `propose_as`.
    AfterPropose,
    /// After the dry run, before the approval is asked.
    AfterDryRun,
    /// Top of every live step loop iteration.
    LoopStart,
    /// After the fresh observation of a live step.
    AfterObserve,
    /// After the (step) approval, before the read / mint.
    AfterApproval,
}

pub struct AgentSupervisor {
    ctl: Arc<KillController>,
    gate: Arc<PolicyGate>,
    clock: Arc<dyn Clock>,
    audit: Arc<dyn AuditSink>,
    broker: ApprovalBroker,
    driver: Arc<dyn AgentDriver>,
    factory: Arc<dyn EngineFactory>,
    engine_killer: Arc<ProcessEngineKiller>,
    settings: Arc<dyn AgentSettings>,
    deny: Arc<dyn DenyProvider>,
    catalogue: Arc<dyn Catalogue + Send + Sync>,
    facts: Arc<dyn BindFacts>,
    notifier: Arc<dyn Notifier>,
    pace: Arc<dyn Pace>,
    interactive_limits: RunLimits,
    unattended_limits: RunLimits,
    next_task: AtomicU64,
    running: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    last_mint: Mutex<Option<Duration>>,
    #[cfg(test)]
    pub(crate) hooks: TestHooks,
}

impl std::fmt::Debug for AgentSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AgentSupervisor(running: {})",
            self.running.load(Ordering::SeqCst)
        )
    }
}

/// Frees the one-task-at-a-time flag on every exit path.
struct RunGuard(Arc<AtomicBool>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Keeps the task's engine attached to the kill switch; detaches on every exit path.
struct AttachGuard(Arc<ProcessEngineKiller>);
impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.0.detach();
    }
}

/// The kill controller's runner abort: a flag the run loop reads between phases.
struct AbortFlag(Arc<AtomicBool>);
impl RunnerHandle for AbortFlag {
    fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Progress {
    task: u64,
    dry_run: bool,
    would_do: u16,
    steps_done: u16,
    executed: u16,
    live: bool,
    answers: Vec<ReadAnswer>,
}

/// Per-task constants.
struct RunCtx {
    task: policy::TaskId,
    epoch0: u64,
    started: Duration,
    deny: Arc<dyn AppDenyPolicy>,
    limits: RunLimits,
}

/// What `settle` decided.
enum Verdict {
    Approved,
    End(TaskOutcome),
}

fn op_kind(s: &PlanStep) -> OpKind {
    match s {
        PlanStep::Observe => OpKind::Observe,
        PlanStep::FindText { .. } => OpKind::FindText,
        PlanStep::AnswerFromScreen { .. } => OpKind::AnswerFromScreen,
        PlanStep::Scroll { .. } => OpKind::Scroll,
        PlanStep::FocusWindow { .. } => OpKind::FocusWindow,
        PlanStep::OpenApp { .. } => OpKind::OpenApp,
        PlanStep::Click { .. } => OpKind::Click,
        PlanStep::Type { .. } => OpKind::Type,
        PlanStep::Submit { .. } => OpKind::Submit,
        PlanStep::KeyCombo(_) => OpKind::KeyCombo,
        PlanStep::KeyChord(_) => OpKind::KeyChord,
        PlanStep::CopySelection => OpKind::CopySelection,
    }
}

/// Typed facts for a summary line: tokens, the (untrusted) target label and the exact text. No
/// model prose exists at this point; the broker cleans the strings again.
fn step_facts(step: &PlanStep, tier: Tier, label: Option<&str>, ask_again: bool) -> StepFacts {
    let mut f = StepFacts::new(op_kind(step), tier).ask_again(ask_again);
    let target = match step {
        PlanStep::OpenApp { name, .. } | PlanStep::FocusWindow { name, .. } => Some(name.as_str()),
        _ => label.or_else(|| step.target().map(policy::Target::label)),
    };
    if let Some(t) = target {
        f = f.target(t);
    }
    if let Some(t) = step.text() {
        f = f.text(t.as_str());
    }
    f
}

fn asks_again(c: policy::Classification) -> bool {
    c.tier == Tier::Consequential && (!c.batchable || c.submit)
}

fn observe_refusal(o: &driver::ObserveOutcome) -> &'static str {
    match o {
        driver::ObserveOutcome::Clear(_) => "ok",
        driver::ObserveOutcome::Withheld(_) => "observation_withheld",
        driver::ObserveOutcome::Paused(_) => "session_paused",
        driver::ObserveOutcome::Blocked(_) => "session_blocked",
    }
}

impl AgentSupervisor {
    /// `Err(SupervisorError::AuditMismatch)` when `parts.audit` is not the sink the gate writes to:
    /// the supervisor's records and the gate's write-ahead intents must land in the SAME
    /// append-only log (fail closed: no supervisor at all).
    pub fn new(parts: SupervisorParts) -> Result<Self, SupervisorError> {
        let gate = Arc::clone(parts.controller.gate());
        let same_sink = std::ptr::eq(
            Arc::as_ptr(&parts.audit).cast::<()>(),
            Arc::as_ptr(&gate.audit_sink()).cast::<()>(),
        );
        if !same_sink {
            return Err(SupervisorError::AuditMismatch);
        }
        let clock = gate.clock();
        Ok(Self {
            ctl: parts.controller,
            gate,
            clock,
            audit: parts.audit,
            broker: parts.broker,
            driver: parts.driver,
            factory: parts.factory,
            engine_killer: Arc::new(ProcessEngineKiller::new()),
            settings: parts.settings,
            deny: parts.deny,
            catalogue: parts.catalogue,
            facts: parts.facts,
            notifier: parts.notifier,
            pace: parts.pace,
            interactive_limits: parts.interactive_limits,
            unattended_limits: parts.unattended_limits,
            next_task: AtomicU64::new(0),
            running: Arc::new(AtomicBool::new(false)),
            abort: Arc::new(AtomicBool::new(false)),
            last_mint: Mutex::new(None),
            #[cfg(test)]
            hooks: TestHooks::default(),
        })
    }

    /// A task is running (one at a time).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// The one gate (`KillController::gate()`).
    pub fn gate(&self) -> &Arc<PolicyGate> {
        &self.gate
    }

    /// S2-13 follow-up: plug the engine, the driver and the runner into the kill controller. Call
    /// once at start-up. After it, one trip kills the engine process (`ProcessEngineKiller`),
    /// releases the driver twice (`Driver::emergency_release`), aborts the run loop and (through
    /// the gate hook and the broker hook) voids every grant and resolves every pending approval
    /// `Killed`.
    pub fn wire_kill(&self) {
        self.ctl.set_engine_killer(self.engine_killer.clone());
        self.ctl
            .set_driver_releaser(Arc::new(DriverRelease(Arc::clone(&self.driver))));
        self.ctl
            .set_runner(Arc::new(AbortFlag(Arc::clone(&self.abort))));
    }

    /// Runs the due scheduled tasks, one after the other (one task at a time). There is no
    /// scheduler thread: the caller decides when to ask.
    pub async fn run_due(&self, src: &dyn ScheduleSource) -> Vec<TaskReport> {
        let mut out = Vec::new();
        for t in src.due(self.clock.now()) {
            out.push(self.run_task(t.into_request()).await);
        }
        out
    }

    // ---------------------------------------------------------------------------------------
    // run_task
    // ---------------------------------------------------------------------------------------

    /// Runs one task to its end and reports how it ended. Never panics on bad input; every
    /// failure is a typed outcome and an audit record.
    pub async fn run_task(&self, req: TaskRequest) -> TaskReport {
        // One task at a time (v1). The refusal touches nothing of the running task.
        if self.running.swap(true, Ordering::SeqCst) {
            let o = TaskOutcome::Refused(RefuseReason::Busy);
            self.log("sup_refused", None, |r| {
                r.decision = Some(o.kind());
                r.result_code = Some(o.detail());
            });
            return TaskReport::refused(RefuseReason::Busy);
        }
        let _free = RunGuard(Arc::clone(&self.running));
        self.abort.store(false, Ordering::SeqCst);
        let mut pr = Progress::default();
        let outcome = self.run_inner(&req, &mut pr).await;
        self.finish(&req, &pr, outcome)
    }

    /// Ends the gate's task when it is still live (a pause, a limit, a refusal after `propose`),
    /// writes the end record and notifies.
    fn finish(&self, req: &TaskRequest, pr: &Progress, outcome: TaskOutcome) -> TaskReport {
        if self.gate.current_task().is_some() {
            // Nothing resumes: the task is voided, the mode is Observe, the busy token is free.
            self.gate.set_observe();
        }
        let task = (pr.task != 0).then_some(pr.task);
        self.log(
            if matches!(outcome, TaskOutcome::Refused(_)) {
                "sup_refused"
            } else {
                "sup_task_end"
            },
            task,
            |r| {
                r.decision = Some(outcome.kind());
                r.result_code = Some(outcome.detail());
            },
        );
        let _ = req;
        TaskReport {
            task: pr.task,
            outcome,
            dry_run: pr.dry_run,
            would_do: pr.would_do,
            steps_done: pr.steps_done,
            executed: pr.executed,
            live: pr.live,
            answers: pr.answers.clone(),
        }
    }

    async fn run_inner(&self, req: &TaskRequest, pr: &mut Progress) -> TaskOutcome {
        use TaskOutcome::{Killed, PlanFailed, PlanRejected, Refused};

        // 1. Fail closed before anything.
        if !self.settings.agent_enabled() {
            return Refused(RefuseReason::AgentDisabled);
        }
        if !self.settings.activity_probe_installed() {
            return Refused(RefuseReason::NoActivityProbe);
        }
        // A kill that is still being carried out (engine, runner, gate, driver) blocks a new task.
        if !self.ctl.wait_idle(Duration::ZERO) {
            return Refused(RefuseReason::Killed);
        }
        if !self.gate.activity().is_idle() {
            return Refused(RefuseReason::Busy);
        }
        if req.transcript.trim().is_empty() || req.transcript.len() > agent::MAX_TRANSCRIPT_BYTES {
            return Refused(RefuseReason::BadRequest);
        }
        let Some(deny) = self.deny.current() else {
            return Refused(RefuseReason::NoDenyList);
        };
        let id = self.next_task.fetch_add(1, Ordering::SeqCst) + 1;
        pr.task = id;
        let parts = match self.factory.create(agent::TaskId(id)) {
            Ok(p) => p,
            Err(e) => return Self::engine_refusal(e),
        };
        let ctx = RunCtx {
            task: policy::TaskId(id),
            epoch0: self.gate.kill_switch().epoch(),
            started: self.clock.now(),
            deny,
            limits: if req.origin.unattended() {
                self.unattended_limits
            } else {
                self.interactive_limits
            },
        };

        // 2. Plan: transcript + system context ONLY. The engine is called once.
        let attach = AttachGuard(Arc::clone(&self.engine_killer));
        self.engine_killer.attach(Arc::clone(&parts.engine));
        let foreground = self.foreground_app(&ctx.deny).await;
        let input = PlanInput {
            task_id: agent::TaskId(id),
            transcript: Arc::from(req.transcript.as_str()),
            context: SystemContext {
                foreground_app: foreground,
                ..SystemContext::new()
            },
            limits: if self.settings.cloud_planning() {
                PlanLimits::cloud()
            } else {
                PlanLimits::local()
            },
        };
        // A kill that landed after `attach` (or before it: nothing was attached to stop) and
        // before the engine is asked to plan must not start the plan.
        #[cfg(test)]
        self.hook(HookPoint::BeforePlan);
        if !self.alive(ctx.epoch0) {
            if let Some(g) = &parts.core_gateway {
                g.kill();
            }
            self.engine_killer.kill_engine_now();
            return Killed(Stage::Planning);
        }
        let planned = parts.engine.plan(input, Arc::clone(&parts.gateway)).await;
        drop(attach);
        if !self.alive(ctx.epoch0) {
            if let Some(g) = &parts.core_gateway {
                g.kill();
            }
            return Killed(Stage::Planning);
        }
        let draft = match planned {
            Ok(PlanOutcome::Draft(d)) => d,
            Ok(PlanOutcome::CannotPlan(_)) => return Refused(RefuseReason::CannotPlan),
            Err(e @ agent::EngineError::Unavailable(_)) => return Self::engine_refusal(e),
            Err(agent::EngineError::Killed) => return PlanFailed("engine_killed"),
            Err(agent::EngineError::Protocol(_)) => return PlanFailed("engine_protocol"),
            Err(agent::EngineError::Timeout) => return PlanFailed("engine_timeout"),
            Err(agent::EngineError::Gateway(_)) => return PlanFailed("engine_gateway"),
            Err(agent::EngineError::LimitExceeded(_)) => return PlanFailed("engine_limit"),
            Err(agent::EngineError::Cancelled) => return PlanFailed("engine_cancelled"),
        };

        // 3. Policy is the authority: strict re-validation of the untrusted draft.
        let plan = {
            let cat: &dyn Catalogue = &*self.catalogue;
            match self.gate.propose_as(
                ctx.task,
                req.origin.policy_origin(),
                draft.as_str(),
                &PlanContext::new(&req.transcript, cat),
            ) {
                Ok(p) => p,
                Err(e) => {
                    return PlanRejected(e.code());
                }
            }
        };
        drop(draft);
        #[cfg(test)]
        self.hook(HookPoint::AfterPropose);
        if !self.alive(ctx.epoch0) {
            return Killed(Stage::Planning);
        }

        // 4. Dry run: inert (no mint, no driver call, nothing but audit records).
        match self.observe(&ctx.deny).await {
            Ok(driver::ObserveOutcome::Clear(_)) => {}
            Ok(_) | Err(()) => return Refused(RefuseReason::ObservationUnavailable),
        }
        let Some(report) = peek(&*self.driver, |o| {
            let extras = self.facts.extras(plan.steps(), o, &*ctx.deny);
            self.gate.dry_run(&plan, o, &self.bind_ctx(o, extras, 0))
        }) else {
            return Refused(RefuseReason::ObservationUnavailable);
        };
        pr.dry_run = true;
        let mut facts = Vec::with_capacity(plan.len());
        let mut refused_step = false;
        for (i, s) in report.steps().iter().enumerate() {
            let step = &plan.steps()[i];
            let prov = plan.provisional()[i];
            let (tier, label, ask) = match s {
                DryRunStep::Bound(b) => {
                    let c = b.classification();
                    self.log("sup_would_do", Some(ctx.task.0), |r| {
                        r.step = Some(i as u16);
                        r.op = Some(step.op_name());
                        r.tier = Some(c.tier.as_str());
                        r.decision = Some("would_do");
                    });
                    (c.tier, Some(b.label().to_owned()), asks_again(c))
                }
                DryRunStep::Unbound(e) => {
                    self.log("sup_would_do", Some(ctx.task.0), |r| {
                        r.step = Some(i as u16);
                        r.op = Some(step.op_name());
                        r.tier = Some(prov.tier.as_str());
                        r.decision = Some("unbound");
                        r.result_code = Some(e.code());
                    });
                    // Never shown bound: whatever it binds to later is asked again.
                    (prov.tier, None, prov.tier >= Tier::Consequential)
                }
            };
            pr.would_do += 1;
            refused_step |= tier == Tier::Never;
            facts.push(step_facts(step, tier, label.as_deref(), ask));
        }
        if refused_step {
            // Stricter than the ADR (which shows a Never step as refused and runs the rest): a
            // plan with a refused step is never offered for real.
            return Refused(RefuseReason::PlanRefusedStep);
        }
        #[cfg(test)]
        self.hook(HookPoint::AfterDryRun);
        if !self.alive(ctx.epoch0) {
            return Killed(Stage::PlanApproval);
        }

        // 5. Plan approval. With observe mode on this IS the "Run it for real?" question that
        //    follows the dry run: live for THIS task only, never a standing grant.
        let offer = self.settings.observe_mode();
        let mut grant = match self.approve_plan(&plan, &report, &facts, offer).await {
            Ok(g) => g,
            Err(o) => return o,
        };
        pr.live = true;
        // Live repeats exactly the dry-run plan. This is guaranteed by construction, not by a
        // compare: there is ONE frozen `ProposedPlan` value (dry run, approval digest and every
        // live step read it), `approve_plan` refuses a grant whose digest differs from the digest
        // the human was shown, and the gate refuses any step that is not the plan's next step
        // (`out_of_plan`, mapped to `Blocked("outside_plan")`). Defence in depth, all in the gate.

        // 6. Live: per frozen step.
        let mut run = 0u16;
        for i in 0..plan.len() {
            #[cfg(test)]
            self.hook(HookPoint::LoopStart);
            if !self.alive(ctx.epoch0) {
                return Killed(Stage::Run);
            }
            if self.clock.now().saturating_sub(ctx.started) > ctx.limits.time_limit {
                return self.stop_limit(pr.task, "time_limit");
            }
            if pr.steps_done >= ctx.limits.max_steps {
                return self.stop_limit(pr.task, "step_budget");
            }
            let planned = &plan.steps()[i];
            run = if i > 0 && *planned == plan.steps()[i - 1] {
                run + 1
            } else {
                1
            };
            if run > ctx.limits.max_repeat {
                return self.stop_limit(pr.task, "runaway");
            }
            #[cfg(test)]
            let injected = lock(&self.hooks.step_override)
                .as_ref()
                .filter(|(at, _)| *at == i)
                .map(|(_, s)| s.clone());
            #[cfg(test)]
            let step = injected.as_ref().unwrap_or(planned);
            #[cfg(not(test))]
            let step = planned;
            if let Err(o) = self.live_step(&ctx, &mut grant, i, step, pr).await {
                return o;
            }
        }
        TaskOutcome::Completed
    }

    // ---------------------------------------------------------------------------------------
    // phases
    // ---------------------------------------------------------------------------------------

    fn engine_refusal(e: agent::EngineError) -> TaskOutcome {
        match e {
            agent::EngineError::Unavailable("unsupported_platform" | "sandbox_failed") => {
                TaskOutcome::Refused(RefuseReason::SandboxUnavailable)
            }
            _ => TaskOutcome::Refused(RefuseReason::EngineUnavailable),
        }
    }

    /// The kill switch has not moved and the runner was not aborted.
    fn alive(&self, epoch0: u64) -> bool {
        !self.abort.load(Ordering::SeqCst) && !self.gate.kill_switch().is_tripped_since(epoch0)
    }

    fn bind_ctx(&self, o: &firewall::Observation, e: BindExtras, idx: u16) -> BindContext {
        let mut c = BindContext::new(
            self.gate.policy_context().clone(),
            SessionState::Active,
            o.id(),
            self.clock.now(),
        );
        c.step_index = idx;
        c.higher_integrity = e.higher_integrity;
        c.focused_secure = e.focused_secure;
        c.target_denied = e.target_denied;
        c.focus_target = e.focus_target;
        c
    }

    fn log(&self, event: &'static str, task: Option<u64>, f: impl FnOnce(&mut AuditRecord)) {
        let mut r = AuditRecord {
            ts_ms: u64::try_from(self.clock.now().as_millis()).unwrap_or(u64::MAX),
            task,
            // The file sink assigns its own strictly increasing sequence.
            seq: 0,
            event,
            plan_digest8: None,
            action_digest8: None,
            step: None,
            op: None,
            target_role: None,
            target_label: None,
            app_id: None,
            tier: None,
            decision: None,
            result_code: None,
            text_len: None,
        };
        f(&mut r);
        self.audit.record(&r);
    }

    /// `Firewall::observe` through the driver on a blocking thread. `Err` = the worker was lost.
    async fn observe(&self, deny: &Arc<dyn AppDenyPolicy>) -> Result<driver::ObserveOutcome, ()> {
        let drv = Arc::clone(&self.driver);
        let deny = Arc::clone(deny);
        tokio::task::spawn_blocking(move || drv.observe(&*deny))
            .await
            .map_err(|_| ())
    }

    /// The foreground app id for the planner: only when the text verdict was Clear and the app is
    /// not deny-listed. Nothing else of the observation is used.
    async fn foreground_app(&self, deny: &Arc<dyn AppDenyPolicy>) -> Option<Arc<str>> {
        match self.observe(deny).await {
            Ok(driver::ObserveOutcome::Clear(_)) => {}
            _ => return None,
        }
        peek(&*self.driver, |o| o.app_id().as_str().to_owned())
            .filter(|a| !deny.is_denied(a))
            .map(Arc::from)
    }

    fn stop_limit(&self, task: u64, why: &'static str) -> TaskOutcome {
        self.log("sup_limit", Some(task), |r| {
            r.decision = Some("stopped");
            r.result_code = Some(why);
        });
        self.notifier.notify(AgentNotice::Stopped { task, why });
        // The runaway / limit detector's own entry point: everything is voided and stopped.
        self.ctl.trip_limit();
        TaskOutcome::Stopped(why)
    }

    fn map_refusal(&self, r: Refusal, c: &RunCtx, step: Option<&PlanStep>) -> TaskOutcome {
        if !self.alive(c.epoch0) {
            return TaskOutcome::Killed(Stage::Run);
        }
        match r {
            Refusal::Killed => TaskOutcome::Killed(Stage::Run),
            Refusal::Paused | Refusal::UnattendedConsequential | Refusal::ScheduledReadOnly => {
                self.log("sup_paused", Some(c.task.0), |rec| {
                    rec.decision = Some("paused");
                    rec.result_code = Some(r.code());
                    rec.op = step.map(PlanStep::op_name);
                });
                self.notifier
                    .notify(AgentNotice::NeedsUser { task: c.task.0 });
                TaskOutcome::Paused(PauseReason::NeedsUser)
            }
            Refusal::OutOfPlan | Refusal::WrongStepIndex => {
                self.log("sup_outside_plan", Some(c.task.0), |rec| {
                    rec.decision = Some("blocked");
                    rec.result_code = Some("outside_plan");
                    rec.op = step.map(PlanStep::op_name);
                });
                TaskOutcome::Blocked("outside_plan")
            }
            other => TaskOutcome::Blocked(other.code()),
        }
    }

    /// Asks the broker and maps the human's answer onto the gate. Common to plan and step
    /// approvals: the gate approval stays pending until this has `decide`d it.
    #[allow(clippy::too_many_arguments)]
    fn settle(
        &self,
        gate_id: policy::ApprovalId,
        gate_hex: &str,
        shown: &ApprovalDigest,
        outcome: ApprovalOutcome,
        stage: Stage,
    ) -> Verdict {
        let deny_gate = || {
            // Always accepted (the safe answer); ends the task.
            let _ = self.gate.decide(gate_id, gate_hex, Decision::Deny);
        };
        match outcome {
            ApprovalOutcome::Approved => {
                // The digest the HUMAN saw (the broker's) goes to the gate: if it is not the
                // gate's digest the gate refuses and the task aborts. No mark_shown here: the
                // broker already enforced its own 500 ms guard from the real display.
                match self.gate.decide(gate_id, shown.hex(), Decision::Approve) {
                    Ok(()) => Verdict::Approved,
                    Err(GateError::DigestMismatch) => {
                        deny_gate();
                        Verdict::End(TaskOutcome::Blocked("digest_mismatch"))
                    }
                    Err(GateError::Voided) => Verdict::End(TaskOutcome::Killed(stage)),
                    Err(e) => {
                        deny_gate();
                        Verdict::End(TaskOutcome::Blocked(e.code()))
                    }
                }
            }
            ApprovalOutcome::Denied => {
                deny_gate();
                Verdict::End(TaskOutcome::Denied(stage))
            }
            ApprovalOutcome::Expired => {
                deny_gate();
                Verdict::End(TaskOutcome::Expired(stage))
            }
            ApprovalOutcome::Killed => Verdict::End(TaskOutcome::Killed(stage)),
        }
    }

    #[cfg_attr(not(test), allow(unused_mut))]
    fn broker_digest(&self, gate_digest_hex: &str) -> ApprovalDigest {
        #[allow(unused_mut)]
        let mut hex = gate_digest_hex.to_owned();
        #[cfg(test)]
        if self.hooks.digest_tamper.load(Ordering::SeqCst) {
            let last = hex.pop().unwrap_or('0');
            hex.push(if last == '0' { '1' } else { '0' });
        }
        ApprovalDigest::from_hex(&hex).unwrap_or_else(|| {
            // The gate's digest is always 64 hex digits; anything else cannot be approved.
            ApprovalDigest::from_hex(&"0".repeat(64)).expect("valid hex")
        })
    }

    async fn approve_plan(
        &self,
        plan: &ProposedPlan,
        report: &DryRunReport,
        facts: &[StepFacts],
        offer: bool,
    ) -> Result<PlanGrant, TaskOutcome> {
        let stage = if offer {
            Stage::LiveOffer
        } else {
            Stage::PlanApproval
        };
        let pending = self
            .gate
            .request_plan_approval(plan, report)
            .map_err(|e| TaskOutcome::Blocked(e.code()))?;
        let (gate_id, gate_hex, gate_digest) =
            (pending.id(), pending.digest_hex(), pending.digest());
        let shown = self.broker_digest(&gate_hex);
        let asked = if offer {
            self.broker
                .request_live_offer(&shown, facts, Some(PLAN_APPROVAL_TIMEOUT))
        } else {
            self.broker
                .request_plan(&shown, facts, true, Some(PLAN_APPROVAL_TIMEOUT))
        };
        let Ok(request) = asked else {
            let _ = self.gate.decide(gate_id, &gate_hex, Decision::Deny);
            return Err(TaskOutcome::Blocked("approval_unavailable"));
        };
        let outcome = Self::wait_broker(request).await;
        match self.settle(gate_id, &gate_hex, &shown, outcome, stage) {
            Verdict::End(o) => Err(o),
            Verdict::Approved => {
                let granted = tokio::task::spawn_blocking(move || pending.wait())
                    .await
                    .map_err(|_| TaskOutcome::Blocked("worker_lost"))?;
                let grant = granted.map_err(|e| match e {
                    GateError::Voided => TaskOutcome::Killed(stage),
                    other => TaskOutcome::Blocked(other.code()),
                })?;
                if grant.plan_digest() != gate_digest {
                    return Err(TaskOutcome::Blocked("digest_mismatch"));
                }
                Ok(grant)
            }
        }
    }

    async fn approve_step(
        &self,
        c: &RunCtx,
        bound: &BoundStep,
        step: &PlanStep,
    ) -> Result<StepGrant, TaskOutcome> {
        let stage = Stage::StepApproval;
        let pending = self
            .gate
            .request_step_approval(bound)
            .map_err(|e| match e {
                GateError::Refused(r) => self.map_refusal(r, c, Some(step)),
                other => TaskOutcome::Blocked(other.code()),
            })?;
        let (gate_id, gate_hex) = (pending.id(), pending.digest_hex());
        let shown = self.broker_digest(&gate_hex);
        let cls = bound.classification();
        // Not required by the tier itself: the step changed since the plan was shown (drift).
        let changed = !asks_again(cls);
        let label = bound.descriptor().and_then(|d| d.label.as_deref());
        let sf = step_facts(step, cls.tier, label, true);
        let asked = self.broker.request_step(
            &shown,
            usize::from(bound.index()),
            &sf,
            cls.submit,
            true,
            changed,
            Some(STEP_APPROVAL_TIMEOUT),
        );
        let Ok(request) = asked else {
            let _ = self.gate.decide(gate_id, &gate_hex, Decision::Deny);
            return Err(TaskOutcome::Blocked("approval_unavailable"));
        };
        let outcome = Self::wait_broker(request).await;
        match self.settle(gate_id, &gate_hex, &shown, outcome, stage) {
            Verdict::End(o) => Err(o),
            Verdict::Approved => {
                let granted = tokio::task::spawn_blocking(move || pending.wait())
                    .await
                    .map_err(|_| TaskOutcome::Blocked("worker_lost"))?;
                granted.map_err(|e| match e {
                    GateError::Voided => TaskOutcome::Killed(stage),
                    other => TaskOutcome::Blocked(other.code()),
                })
            }
        }
    }

    /// `PendingApproval::wait` blocks (condvar): it runs on a blocking thread, never in async
    /// code. A lost worker is a Deny.
    async fn wait_broker(request: PendingApproval) -> ApprovalOutcome {
        tokio::task::spawn_blocking(move || request.wait())
            .await
            .unwrap_or(ApprovalOutcome::Denied)
    }

    /// Leaves the gate's 250 ms floor between two mints.
    async fn pace_mint(&self) {
        let last = *lock(&self.last_mint);
        if let Some(l) = last {
            let since = self.clock.now().saturating_sub(l);
            if since < MINT_GAP {
                self.pace.pause(MINT_GAP - since).await;
            }
        }
    }

    /// One live step: fresh observation, bind, verdict, [approval], mint, execute, result.
    async fn live_step(
        &self,
        c: &RunCtx,
        grant: &mut PlanGrant,
        i: usize,
        step: &PlanStep,
        pr: &mut Progress,
    ) -> Result<(), TaskOutcome> {
        use TaskOutcome::{Blocked, Failed, Killed};
        self.pace_mint().await;
        #[cfg(test)]
        let reuse = *lock(&self.hooks.reuse_observation_at) == Some(i);
        #[cfg(not(test))]
        let reuse = false;
        if !reuse {
            // A fresh observation for EVERY step: an observation authorises one action.
            match self.observe(&c.deny).await {
                Ok(driver::ObserveOutcome::Clear(_)) => {}
                Ok(other) => return Err(Blocked(observe_refusal(&other))),
                Err(()) => return Err(Blocked("observe_failed")),
            }
        }
        #[cfg(test)]
        self.hook(HookPoint::AfterObserve);
        if !self.alive(c.epoch0) {
            return Err(Killed(Stage::Run));
        }

        let bound = peek(&*self.driver, |o| {
            if let Err(e) = self.gate.note_observation(o.id(), SessionState::Active) {
                return Err(e.code());
            }
            let extras = self.facts.extras(std::slice::from_ref(step), o, &*c.deny);
            policy::bind_step(step, o, &self.bind_ctx(o, extras, i as u16)).map_err(|e| e.code())
        });
        let bound = match bound {
            None => return Err(Blocked("no_observation")),
            Some(Err(code)) => return Err(Blocked(code)),
            Some(Ok(b)) => b,
        };

        let single = match self.gate.evaluate(&bound, Some(grant)) {
            Evaluation::Allow => None,
            Evaluation::NeedsApproval => Some(self.approve_step(c, &bound, step).await?),
            Evaluation::Deny(r) => return Err(self.map_refusal(r, c, Some(step))),
            Evaluation::WouldDo(_) => return Err(Blocked("observe_mode")),
        };
        #[cfg(test)]
        self.hook(HookPoint::AfterApproval);
        if !self.alive(c.epoch0) {
            return Err(Killed(Stage::Run));
        }

        // Read steps (observe, find_text, answer_from_screen) never reach the driver.
        if bound.action().is_none() {
            self.gate
                .complete_read(&bound, grant)
                .map_err(|r| self.map_refusal(r, c, Some(step)))?;
            // S2-18: the result of a read, from the cleared text of THIS step's observation.
            let read = match step {
                PlanStep::FindText { query } => Some((ReadKind::FindText, query)),
                PlanStep::AnswerFromScreen { query } => Some((ReadKind::AnswerFromScreen, query)),
                _ => None,
            };
            if let Some((kind, query)) = read {
                if pr.answers.len() < MAX_ANSWERS {
                    if let Some(a) = peek(&*self.driver, |o| {
                        ReadAnswer::from_screen(kind, query, o.text())
                    }) {
                        pr.answers.push(a);
                    }
                }
            }
            pr.steps_done += 1;
            return Ok(());
        }

        // Write-ahead intent is written by `authorize` before it mints.
        let approved = self
            .gate
            .authorize(bound, grant, single)
            .map_err(|r| self.map_refusal(r, c, Some(step)))?;
        *lock(&self.last_mint) = Some(self.clock.now());
        let digest = approved.digest();
        let drv = Arc::clone(&self.driver);
        let deny = Arc::clone(&c.deny);
        let kill: KillSwitch = self.gate.kill_switch().clone();
        let out =
            match tokio::task::spawn_blocking(move || drv.execute(approved, &*deny, &kill)).await {
                Ok(o) => o,
                Err(_) => {
                    self.gate
                        .record_result(digest, &ExecResult::failed("worker_lost"));
                    return Err(Failed("worker_lost"));
                }
            };
        // Exactly once; a failure ends the gate's task.
        self.gate.record_result(digest, &out.exec_result());
        match out {
            driver::ExecOutcome::Executed(_) => {
                pr.executed += 1;
                pr.steps_done += 1;
                Ok(())
            }
            _ if !self.alive(c.epoch0) => Err(Killed(Stage::Run)),
            driver::ExecOutcome::Refused(e) => Err(Blocked(e.code())),
            driver::ExecOutcome::Failed { error, .. } => Err(Failed(error.code())),
        }
    }
}

#[cfg(test)]
impl AgentSupervisor {
    fn hook(&self, p: HookPoint) {
        let f = lock(&self.hooks.at)
            .as_ref()
            .filter(|(q, _)| *q == p)
            .map(|(_, f)| Arc::clone(f));
        if let Some(f) = f {
            f();
        }
    }
    pub(crate) fn engine_killer(&self) -> &Arc<ProcessEngineKiller> {
        &self.engine_killer
    }
    pub(crate) fn aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }
}
