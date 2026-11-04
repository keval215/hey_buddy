//! Plain data and the small traits of the agent supervisor (S2-17 + S2-23). Everything here is a
//! fixed token or a number: `Debug` never carries the transcript, labels, typed text or model
//! output (L-010, L-022).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agent::{AgentEngine, EngineError, ModelGateway};
use async_trait::async_trait;
use frame::AppDenyPolicy;
use policy::TaskOrigin;

use crate::buddy::Buddy;
use crate::gateway::CoreModelGateway;

// ---------------------------------------------------------------------------------------------
// Request and report
// ---------------------------------------------------------------------------------------------

/// Who started the task. Maps onto the gate's origin with STRICTER WINS: `Scheduled` is the gate's
/// Read-only `Scheduled`, `Manual` is the gate's `Background` (Read + Reversible, pauses at the
/// first Consequential step), `Interactive` runs normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOrigin {
    /// The user asked by voice, in person.
    Interactive,
    /// A timer fired (`ScheduleSource`).
    Scheduled,
    /// The user pressed "run now" on a stored task and walked away (`ManualTrigger`).
    Manual,
}

impl RequestOrigin {
    pub fn code(self) -> &'static str {
        match self {
            RequestOrigin::Interactive => "interactive",
            RequestOrigin::Scheduled => "scheduled",
            RequestOrigin::Manual => "manual",
        }
    }

    /// The policy origin (the gate additionally takes the stricter of this and its own config).
    pub fn policy_origin(self) -> TaskOrigin {
        match self {
            RequestOrigin::Interactive => TaskOrigin::Interactive,
            RequestOrigin::Scheduled => TaskOrigin::Scheduled,
            RequestOrigin::Manual => TaskOrigin::Background,
        }
    }

    /// Nobody is watching: the stricter limits apply.
    pub fn unattended(self) -> bool {
        !matches!(self, RequestOrigin::Interactive)
    }
}

/// One request to run a task. The transcript is the user's words; it is never printed.
#[derive(Clone)]
pub struct TaskRequest {
    pub origin: RequestOrigin,
    pub transcript: String,
}

impl TaskRequest {
    pub fn new(origin: RequestOrigin, transcript: impl Into<String>) -> Self {
        Self {
            origin,
            transcript: transcript.into(),
        }
    }
    pub fn interactive(transcript: impl Into<String>) -> Self {
        Self::new(RequestOrigin::Interactive, transcript)
    }
}

impl fmt::Debug for TaskRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TaskRequest({}, {} B)",
            self.origin.code(),
            self.transcript.len()
        )
    }
}

/// Why a task was refused before anything ran. Fixed tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    /// `agent_enabled` is off (no writable audit log, or no kill trigger registered).
    AgentDisabled,
    /// No activity probe is installed on the buddy.
    NoActivityProbe,
    /// A kill is still being carried out.
    Killed,
    /// The engine sandbox is unavailable (`unsupported_platform`, `sandbox_failed`).
    SandboxUnavailable,
    /// The engine could not be created or started.
    EngineUnavailable,
    /// Another task is running (one at a time in v1).
    Busy,
    /// Empty or oversize request.
    BadRequest,
    /// No deny list is available: nothing can be observed safely.
    NoDenyList,
    /// The screen cannot be observed (withheld, session paused or blocked).
    ObservationUnavailable,
    /// The engine said it cannot plan this request.
    CannotPlan,
    /// The dry run shows a step that is refused outright (Never tier).
    PlanRefusedStep,
}

impl RefuseReason {
    pub fn code(self) -> &'static str {
        match self {
            RefuseReason::AgentDisabled => "agent_disabled",
            RefuseReason::NoActivityProbe => "no_activity_probe",
            RefuseReason::Killed => "killed",
            RefuseReason::SandboxUnavailable => "sandbox_unavailable",
            RefuseReason::EngineUnavailable => "engine_unavailable",
            RefuseReason::Busy => "busy",
            RefuseReason::BadRequest => "bad_request",
            RefuseReason::NoDenyList => "no_deny_list",
            RefuseReason::ObservationUnavailable => "observation_unavailable",
            RefuseReason::CannotPlan => "cannot_plan",
            RefuseReason::PlanRefusedStep => "plan_refused_step",
        }
    }
}

/// Where in the flow an approval was asked / the task stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Planning,
    PlanApproval,
    /// "Run it for real?" after the dry run.
    LiveOffer,
    StepApproval,
    /// Between phases of a step, or inside the driver.
    Run,
}

impl Stage {
    pub fn code(self) -> &'static str {
        match self {
            Stage::Planning => "planning",
            Stage::PlanApproval => "plan_approval",
            Stage::LiveOffer => "live_offer",
            Stage::StepApproval => "step_approval",
            Stage::Run => "run",
        }
    }
}

/// An unattended task stopped and needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    NeedsUser,
}

impl PauseReason {
    pub fn code(self) -> &'static str {
        match self {
            PauseReason::NeedsUser => "needs_user",
        }
    }
}

/// How a task ended. Every payload is a fixed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    /// Nothing was started.
    Refused(RefuseReason),
    /// The engine failed (`engine_protocol`, `engine_timeout`, ...).
    PlanFailed(&'static str),
    /// `policy::propose` rejected the engine's draft (a `PlanError` code).
    PlanRejected(&'static str),
    /// The user said no.
    Denied(Stage),
    /// The approval timed out.
    Expired(Stage),
    /// The kill switch fired.
    Killed(Stage),
    /// An unattended task reached a step it may not run: it stops and notifies.
    Paused(PauseReason),
    /// The gate, the firewall or the supervisor refused a step (`outside_plan`, `no_such_element`,
    /// `action_refused`, `digest_mismatch`, ...). Steps that already ran stay run.
    Blocked(&'static str),
    /// The driver failed a step (a `DriverError` code).
    Failed(&'static str),
    /// A limit stopped the task (`time_limit`, `step_budget`, `runaway`).
    Stopped(&'static str),
    /// Every step ran.
    Completed,
}

impl TaskOutcome {
    /// Coarse kind (the audit `decision` field).
    pub fn kind(self) -> &'static str {
        match self {
            TaskOutcome::Refused(_) => "refused",
            TaskOutcome::PlanFailed(_) => "plan_failed",
            TaskOutcome::PlanRejected(_) => "plan_rejected",
            TaskOutcome::Denied(_) => "denied",
            TaskOutcome::Expired(_) => "expired",
            TaskOutcome::Killed(_) => "killed",
            TaskOutcome::Paused(_) => "paused",
            TaskOutcome::Blocked(_) => "blocked",
            TaskOutcome::Failed(_) => "failed",
            TaskOutcome::Stopped(_) => "stopped",
            TaskOutcome::Completed => "completed",
        }
    }

    /// Detail token (the audit `result_code` field).
    pub fn detail(self) -> &'static str {
        match self {
            TaskOutcome::Refused(r) => r.code(),
            TaskOutcome::PlanFailed(c)
            | TaskOutcome::PlanRejected(c)
            | TaskOutcome::Blocked(c)
            | TaskOutcome::Failed(c)
            | TaskOutcome::Stopped(c) => c,
            TaskOutcome::Denied(s) | TaskOutcome::Expired(s) | TaskOutcome::Killed(s) => s.code(),
            TaskOutcome::Paused(p) => p.code(),
            TaskOutcome::Completed => "ok",
        }
    }
}

/// The typed result of `run_task`. No content: counts and tokens only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskReport {
    /// 0 when the task was refused before it got an id.
    pub task: u64,
    pub outcome: TaskOutcome,
    /// The dry run was walked (steps recorded as "would do").
    pub dry_run: bool,
    /// Steps recorded as "would do".
    pub would_do: u16,
    /// Steps completed live (reads and executed actions).
    pub steps_done: u16,
    /// Actions the driver executed.
    pub executed: u16,
    /// The user chose live for this task.
    pub live: bool,
}

impl TaskReport {
    pub fn refused(reason: RefuseReason) -> Self {
        Self {
            task: 0,
            outcome: TaskOutcome::Refused(reason),
            dry_run: false,
            would_do: 0,
            steps_done: 0,
            executed: 0,
            live: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------------------------

/// Bounds of one task. Limits can only tighten (D-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// Most live steps one task may run.
    pub max_steps: u16,
    /// The same step more than this many times in a row is a runaway.
    pub max_repeat: u16,
    /// Wall time (gate clock) from the start of the task.
    pub time_limit: Duration,
}

impl RunLimits {
    /// The user is watching.
    pub fn interactive() -> Self {
        Self {
            max_steps: 12,
            max_repeat: 5,
            time_limit: Duration::from_secs(10 * 60),
        }
    }
    /// Nobody is watching (manual / scheduled): tighter.
    pub fn unattended() -> Self {
        Self {
            max_steps: 8,
            max_repeat: 3,
            time_limit: Duration::from_secs(5 * 60),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Collaborators (all with fakes)
// ---------------------------------------------------------------------------------------------

/// What the settings say. `observe_mode` is ON by default: every task starts as a dry run.
pub trait AgentSettings: Send + Sync {
    /// The agent may run (audit log writable and a kill trigger registered).
    fn agent_enabled(&self) -> bool;
    /// An activity probe is installed.
    fn activity_probe_installed(&self) -> bool;
    /// Dry run first, then "Run it for real?".
    fn observe_mode(&self) -> bool;
    /// The planner talks to a cloud model (45 s plan deadline instead of 60 s).
    fn cloud_planning(&self) -> bool;
}

/// Settings of the real app: the buddy's own state. Observe mode is ON until changed.
pub struct BuddyAgentSettings {
    buddy: Arc<Buddy>,
    observe: std::sync::atomic::AtomicBool,
}

impl BuddyAgentSettings {
    pub fn new(buddy: Arc<Buddy>) -> Self {
        Self {
            buddy,
            observe: std::sync::atomic::AtomicBool::new(true),
        }
    }
    pub fn set_observe_mode(&self, on: bool) {
        self.observe.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

impl AgentSettings for BuddyAgentSettings {
    fn agent_enabled(&self) -> bool {
        self.buddy.agent_enabled()
    }
    fn activity_probe_installed(&self) -> bool {
        self.buddy.has_activity_probe()
    }
    fn observe_mode(&self) -> bool {
        self.observe.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn cloud_planning(&self) -> bool {
        self.buddy.get_settings().provider == crate::api::ProviderChoice::Anthropic
    }
}

/// Fixed answers (tests, and a stand-in).
#[derive(Debug, Clone, Copy)]
pub struct FixedSettings {
    pub agent_enabled: bool,
    pub probe: bool,
    pub observe_mode: bool,
    pub cloud: bool,
}

impl Default for FixedSettings {
    fn default() -> Self {
        Self {
            agent_enabled: true,
            probe: true,
            observe_mode: true,
            cloud: false,
        }
    }
}

impl AgentSettings for FixedSettings {
    fn agent_enabled(&self) -> bool {
        self.agent_enabled
    }
    fn activity_probe_installed(&self) -> bool {
        self.probe
    }
    fn observe_mode(&self) -> bool {
        self.observe_mode
    }
    fn cloud_planning(&self) -> bool {
        self.cloud
    }
}

/// The current deny list. `None` = unavailable: nothing is observed (fail closed).
pub trait DenyProvider: Send + Sync {
    fn current(&self) -> Option<Arc<dyn AppDenyPolicy>>;
}

/// The buddy's deny list (the one the settings screen edits).
pub struct BuddyDeny(pub Arc<Buddy>);

impl DenyProvider for BuddyDeny {
    fn current(&self) -> Option<Arc<dyn AppDenyPolicy>> {
        self.0.current_deny().map(|d| d as Arc<dyn AppDenyPolicy>)
    }
}

/// A fixed policy (tests).
pub struct FixedDeny(pub Arc<dyn AppDenyPolicy>);

impl DenyProvider for FixedDeny {
    fn current(&self) -> Option<Arc<dyn AppDenyPolicy>> {
        Some(Arc::clone(&self.0))
    }
}

/// What the supervisor tells the user without a window (a toast, a spoken line). Fixed tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNotice {
    /// An unattended task paused at a step only the user may approve.
    NeedsUser { task: u64 },
    /// A limit stopped the task.
    Stopped { task: u64, why: &'static str },
}

pub trait Notifier: Send + Sync {
    fn notify(&self, notice: AgentNotice);
}

/// Drops every notice.
pub struct NoNotifier;
impl Notifier for NoNotifier {
    fn notify(&self, _notice: AgentNotice) {}
}

/// Records notices (tests).
#[derive(Default)]
pub struct RecordingNotifier {
    seen: std::sync::Mutex<Vec<AgentNotice>>,
}

impl RecordingNotifier {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn notices(&self) -> Vec<AgentNotice> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, notice: AgentNotice) {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notice);
    }
}

/// "Run now" for a stored task (the user is present when it starts, then may walk away).
#[derive(Clone)]
pub struct ManualTrigger {
    pub transcript: String,
}

impl ManualTrigger {
    pub fn into_request(self) -> TaskRequest {
        TaskRequest::new(RequestOrigin::Manual, self.transcript)
    }
}

impl fmt::Debug for ManualTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ManualTrigger({} B)", self.transcript.len())
    }
}

/// A stored task that runs on a timer. There is NO scheduler thread in v1: something calls
/// [`ScheduleSource::due`] and hands the result to `AgentSupervisor::run_due`.
#[derive(Clone)]
pub struct ScheduledTrigger {
    pub id: u32,
    pub transcript: String,
    pub every: Duration,
}

impl ScheduledTrigger {
    pub fn into_request(self) -> TaskRequest {
        TaskRequest::new(RequestOrigin::Scheduled, self.transcript)
    }
}

impl fmt::Debug for ScheduledTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ScheduledTrigger(#{}, {} B, every {:?})",
            self.id,
            self.transcript.len(),
            self.every
        )
    }
}

/// Where scheduled tasks come from.
pub trait ScheduleSource: Send + Sync {
    /// The triggers due at `now` (gate clock). Each one is returned once.
    fn due(&self, now: Duration) -> Vec<ScheduledTrigger>;
}

/// A scripted schedule (tests): triggers with a fixed due time, returned once.
#[derive(Default)]
pub struct FakeSchedule {
    items: std::sync::Mutex<Vec<(Duration, ScheduledTrigger)>>,
}

impl FakeSchedule {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&self, at: Duration, t: ScheduledTrigger) {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((at, t));
    }
}

impl ScheduleSource for FakeSchedule {
    fn due(&self, now: Duration) -> Vec<ScheduledTrigger> {
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let (due, rest): (Vec<_>, Vec<_>) = g.drain(..).partition(|(at, _)| *at <= now);
        *g = rest;
        due.into_iter().map(|(_, t)| t).collect()
    }
}

/// Sleeps between steps (the gate's 250 ms mint floor). Real time in the app; a manual clock in
/// tests, so they never wait.
#[async_trait]
pub trait Pace: Send + Sync {
    async fn pause(&self, d: Duration);
}

/// `tokio::time::sleep`.
pub struct TokioPace;

#[async_trait]
impl Pace for TokioPace {
    async fn pause(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Engines
// ---------------------------------------------------------------------------------------------

/// A fresh engine and its model gateway for ONE task.
pub struct EngineParts {
    pub engine: Arc<dyn AgentEngine>,
    pub gateway: Arc<dyn ModelGateway>,
    /// The concrete gateway, so a kill can stop its model calls (`CoreModelGateway::kill`).
    pub core_gateway: Option<Arc<CoreModelGateway>>,
}

/// Makes a fresh engine (a new sandboxed process for the real one) and a fresh
/// `CoreModelGateway` (its own 3-call budget) for every task. `Err(Unavailable(..))` refuses the
/// task.
pub trait EngineFactory: Send + Sync {
    fn create(&self, task: agent::TaskId) -> Result<EngineParts, EngineError>;
}

/// The real factory: a `ProcessEngine` over the platform sandbox and a `CoreModelGateway`.
pub struct ProcessEngineFactory {
    pub buddy: Arc<Buddy>,
    pub providers: Arc<dyn crate::turn::ProviderSource>,
    pub sandbox: Arc<dyn agent::EngineSandbox>,
    pub spec: agent::LaunchSpec,
    pub session: u64,
}

impl EngineFactory for ProcessEngineFactory {
    fn create(&self, task: agent::TaskId) -> Result<EngineParts, EngineError> {
        let engine: Arc<dyn AgentEngine> = Arc::new(agent::ProcessEngine::new(
            Arc::clone(&self.sandbox),
            self.spec.clone(),
            self.session,
        ));
        let gw = Arc::new(CoreModelGateway::new(
            Arc::clone(&self.buddy),
            Arc::clone(&self.providers),
            task,
        ));
        Ok(EngineParts {
            engine,
            gateway: gw.clone(),
            core_gateway: Some(gw),
        })
    }
}
