//! The policy gate (ADR 0003 sections 6-9, 11, 13): the ONLY minter of [`ApprovedAction`].
//!
//! # State machine (one task at a time)
//! ```text
//! propose ──► Observe ──dry_run──► request_plan_approval ──decide(Approve)──► wait ──► Live
//!    ▲           │ (no mint, ever)       (digest-bound, 120 s)                  │      (this task only)
//!    │           └── void_all / kill / timeout / deny / task end ◄──────────────┘
//! Live: per frozen step, in order, once each:
//!   note_observation → bind_step → evaluate → [request_step_approval → decide → wait → StepGrant]
//!   → authorize ──► ApprovedAction ──► driver.execute ──► record_result
//!   (Read steps: complete_read; plan-time Never steps: skip_refused)
//! ```
//! Every transition that can end a task (deny, timeout, expiry, Never at execution, kill, failed
//! execution) retires the plan digest for the session and returns the mode to Observe.
//!
//! # Threading
//! All state is behind ONE `Mutex` plus a `Condvar`. Nothing external (audit sink, event sink,
//! clock, kill hooks) is ever called while the lock is held: events and audit records are
//! collected under the lock and delivered after it is released (L-013). The audit INTENT append
//! happens between two short critical sections, protected by a "reserving" busy token that
//! carries the full identity phase A validated (task, proposal, plan digest, step, void
//! generation, kill epoch); phase C re-validates ALL of it. Ending a task never releases a
//! reserving/outstanding token (only `record_result`, `ACTION_TTL`, void or kill do).
//!
//! # Poison and panics
//! A poisoned gate lock fails CLOSED: the gate is marked dead, the void generation moves (every
//! grant and action dies), waiters wake with `Voided`, and every entry point refuses with
//! `Refusal::GatePoisoned`. A panic in an audit/event sink during `authorize` releases the
//! reservation (drop guard) instead of wedging the gate.
//!
//! # Gate identity
//! Every gate has a random id derived from its session nonce. `ProposedPlan`, `DryRunReport`
//! and the grants carry it; another gate refuses them (`WrongPlan`).
//!
//! # `ApprovedAction` cannot be forged
//! These snippets must NOT compile. NOTE: stable rustdoc does NOT enforce the `EXXXX` code in
//! `compile_fail,EXXXX` (it only checks that compilation fails), so the codes below are
//! documentation. They were verified by hand with `rustc` against the built rlib, and
//! `tools/check-doctest-codes.sh` re-checks them (run it after changing these snippets or the
//! struct fields).
//! ```compile_fail,E0451
//! // Private fields: no struct literal from outside the crate.
//! fn f() -> policy::ApprovedAction {
//!     policy::ApprovedAction {
//!         digest: todo!(),
//!         step: todo!(),
//!         plan_digest: todo!(),
//!         task: todo!(),
//!         epoch: todo!(),
//!         kill: todo!(),
//!         void_at: todo!(),
//!         void: todo!(),
//!         clock: todo!(),
//!         minted: todo!(),
//!     }
//! }
//! ```
//! ```compile_fail,E0624
//! // The mint is pub(crate).
//! fn f() -> policy::ApprovedAction {
//!     policy::ApprovedAction::mint(
//!         todo!(), todo!(), todo!(), todo!(), todo!(), todo!(), todo!(), todo!(), todo!(), todo!(),
//!     )
//! }
//! ```
//! ```compile_fail,E0382
//! // Single use: consumed by value.
//! fn f(a: policy::ApprovedAction) { drop(a); drop(a); }
//! ```
//! ```compile_fail,E0599
//! // No Clone (owned receiver, not a reference).
//! fn f(a: policy::ApprovedAction) { let _ = a.clone(); }
//! ```
//! ```compile_fail,E0308
//! // A BoundStep is data, not authority.
//! fn needs_approved(_: policy::ApprovedAction) {}
//! fn f(s: policy::BoundStep) { needs_approved(s); }
//! ```
//! ```compile_fail,E0308
//! // Neither is a StepGrant.
//! fn needs_approved(_: policy::ApprovedAction) {}
//! fn f(g: policy::StepGrant) { needs_approved(g); }
//! ```
//! ```compile_fail,E0277
//! // No Default.
//! fn needs_default<T: Default>() {}
//! needs_default::<policy::ApprovedAction>();
//! ```
//! ```compile_fail,E0277
//! // No From<BoundStep>.
//! fn needs_from<T: From<policy::BoundStep>>() {}
//! needs_from::<policy::ApprovedAction>();
//! ```
//! ```compile_fail,E0451
//! // Grants are sealed too.
//! fn f() -> policy::PlanGrant {
//!     policy::PlanGrant {
//!         gate: todo!(),
//!         task: todo!(),
//!         plan_digest: todo!(),
//!         next_step: todo!(),
//!         epoch: todo!(),
//!         void_gen: todo!(),
//!     }
//! }
//! ```
//! ```compile_fail,E0599
//! // A StepGrant is single use: no Clone.
//! fn f(g: policy::StepGrant) { let _ = g.clone(); }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use action::{action_digest, Action, ActionKind};
use firewall::{AppId, Observation, ObservationId, SessionState};

use crate::audit::{audit_label, AuditRecord, AuditSink};
use crate::bind::{bind_step, BindContext, BoundStep};
use crate::clock::Clock;
use crate::config::{
    Mode, PolicyConfig, TaskOrigin, ACTION_TTL, INPUT_GUARD, MAX_MINTS_PER_MINUTE, MIN_MINT_GAP,
    OBSERVATION_MAX_AGE, PLAN_APPROVAL_TIMEOUT, PLAN_GRANT_TTL, RESERVE_MAX_AGE, STALL_TTL,
    STEP_APPROVAL_TIMEOUT,
};
use crate::drift::{drift, Drift, Shown};
use crate::error::PlanError;
use crate::events::{AgentEvent, AgentEventKind, ApprovalKind, DenyWhy, EventSink, NullEvents};
use crate::facts::{BoundStepFacts, PolicyContext};
use crate::ids::{approval_id, gate_id_of, ActionDigest, ApprovalId, TaskId};
use crate::kill::{KillReason, KillSwitch};
use crate::plan::{validate, PlanContext, PlanDigest, PlanStep};
use crate::report::{DryBound, DryRunReport, DryRunStep, ProposedPlan};
use crate::tier::{Classification, Tier};

// ---------------------------------------------------------------------------------------------
// Public vocabulary
// ---------------------------------------------------------------------------------------------

/// The approval window's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

/// Why `void_all` ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoidReason {
    Killed(KillReason),
    ObserveToggled,
    SettingsChanged,
    DenyListChanged,
    Quit,
}

impl VoidReason {
    pub fn as_str(self) -> &'static str {
        match self {
            VoidReason::Killed(_) => "killed",
            VoidReason::ObserveToggled => "observe_toggled",
            VoidReason::SettingsChanged => "settings_changed",
            VoidReason::DenyListChanged => "deny_list_changed",
            VoidReason::Quit => "quit",
        }
    }
}

/// Why a task ended (the `TaskFinished` outcome token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Completed,
    Denied,
    Timeout,
    Expired,
    Refused,
    Killed,
    Voided,
    ExecFailed,
    Superseded,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Completed => "completed",
            EndReason::Denied => "denied",
            EndReason::Timeout => "timeout",
            EndReason::Expired => "expired",
            EndReason::Refused => "refused",
            EndReason::Killed => "killed",
            EndReason::Voided => "voided",
            EndReason::ExecFailed => "exec_failed",
            EndReason::Superseded => "superseded",
        }
    }
}

/// The busy token as seen from outside (S2-13 `activity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyState {
    Idle,
    /// An approval is waiting for the user.
    Pending,
    /// `authorize` is writing its write-ahead intent.
    Reserving,
    /// A minted action the driver has not reported on.
    Outstanding,
}

/// What the gate is doing right now (read-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activity {
    /// A task exists that has not ended.
    pub task_live: bool,
    pub busy_state: BusyState,
}

impl Activity {
    /// True when nothing is running and nothing is outstanding.
    pub fn is_idle(&self) -> bool {
        !self.task_live && self.busy_state == BusyState::Idle
    }
}

/// Why `authorize` (or `evaluate`) refused. One code per checked condition; fixed tokens only.
/// Also used as `DenyReason` in [`Evaluation::Deny`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// No task is running.
    NoTask,
    /// The task already ended.
    TaskEnded,
    /// The kill switch moved since the task/grant started.
    Killed,
    /// Task is in Observe mode: nothing is ever minted (`WouldDo`).
    ObserveMode,
    /// `void_all` ran since the grant was issued.
    Voided,
    /// The grant is not for the frozen plan.
    WrongPlan,
    /// 10 min since approval, or 30 s without step progress. Ends the task.
    Expired,
    /// An unattended task stopped at a step it may not run.
    Paused,
    /// The step index is not the grant's next step (replay or skip).
    WrongStepIndex,
    /// Row 27: the abstract op differs from the frozen step.
    OutOfPlan,
    /// Observation not current / too old.
    Stale,
    /// The fresh preflight is not Active.
    SessionNotActive,
    /// Never at execution (or Never classified now). Ends the task.
    ActionRefused,
    /// The step has no `action::Action` (read operation or chord outside `KeyCombo`).
    NotAnAction,
    /// Read-only entry point used on a step that is not a Read op.
    NotARead,
    /// Non-interactive task reached a Consequential step. Pauses the task.
    UnattendedConsequential,
    /// Scheduled tasks are Read-only. Pauses the task.
    ScheduledReadOnly,
    /// The step changed since plan approval: needs a single-step approval.
    DriftPause,
    /// Consequential and not covered by the plan grant: needs a `StepGrant`.
    StepApprovalRequired,
    /// The `StepGrant` is not for exactly this step/action digest.
    StepGrantMismatch,
    /// An approval is pending or an `ApprovedAction` is outstanding.
    Busy,
    /// Rate floor (250 ms between mints, 30 per minute).
    RateLimited,
    /// The audit intent append failed: fail closed.
    AuditFailed,
    /// `skip_refused` on a step that was not refused at plan time.
    NotRefused,
    /// The gate's internal lock was poisoned (a panic while it was held): the gate is dead,
    /// fail closed, for the rest of the session.
    GatePoisoned,
}

pub type DenyReason = Refusal;

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::NoTask => "no_task",
            Refusal::TaskEnded => "task_ended",
            Refusal::Killed => "killed",
            Refusal::ObserveMode => "observe_mode",
            Refusal::Voided => "voided",
            Refusal::WrongPlan => "wrong_plan",
            Refusal::Expired => "expired",
            Refusal::Paused => "paused",
            Refusal::WrongStepIndex => "wrong_step_index",
            Refusal::OutOfPlan => "out_of_plan",
            Refusal::Stale => "stale",
            Refusal::SessionNotActive => "session_not_active",
            Refusal::ActionRefused => "action_refused",
            Refusal::NotAnAction => "not_an_action",
            Refusal::NotARead => "not_a_read",
            Refusal::UnattendedConsequential => "unattended_consequential",
            Refusal::ScheduledReadOnly => "scheduled_read_only",
            Refusal::DriftPause => "drift_pause",
            Refusal::StepApprovalRequired => "step_approval_required",
            Refusal::StepGrantMismatch => "step_grant_mismatch",
            Refusal::Busy => "busy",
            Refusal::RateLimited => "rate_limited",
            Refusal::AuditFailed => "audit_failed",
            Refusal::NotRefused => "not_refused",
            Refusal::GatePoisoned => "gate_poisoned",
        }
    }

    /// The step needs a single-step approval rather than being refused.
    pub fn is_approval_request(self) -> bool {
        matches!(self, Refusal::DriftPause | Refusal::StepApprovalRequired)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Refusal {}

/// Errors of the approval plumbing. Fixed tokens only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateError {
    /// The gate could not start (no OS randomness).
    Unavailable,
    NoTask,
    /// The proposal/report does not belong to the current task.
    WrongPlan,
    /// The plan was already approved (frozen) for this task.
    AlreadyApproved,
    /// An approval is pending or an action is outstanding.
    Busy,
    /// This plan digest was retired (task ended, killed): it can never be granted again.
    Retired,
    /// The step has no `action::Action`.
    NotAnAction,
    /// Step approval for a step that is not the next frozen step.
    OutOfPlan,
    /// Step approval for a step that is Never or whose task cannot proceed.
    Refused(Refusal),
    UnknownApproval,
    /// The quoted digest is not the one held (the approval stays pending).
    DigestMismatch,
    /// Decision arrived within 500 ms of the approval being shown; ignored.
    TooEarly,
    /// The user denied.
    Denied,
    Timeout,
    UiUnavailable,
    /// `void_all` / kill woke the waiter.
    Voided,
    Cancelled,
}

impl GateError {
    pub fn code(self) -> &'static str {
        match self {
            GateError::Unavailable => "unavailable",
            GateError::NoTask => "no_task",
            GateError::WrongPlan => "wrong_plan",
            GateError::AlreadyApproved => "already_approved",
            GateError::Busy => "busy",
            GateError::Retired => "retired",
            GateError::NotAnAction => "not_an_action",
            GateError::OutOfPlan => "out_of_plan",
            GateError::Refused(r) => r.code(),
            GateError::UnknownApproval => "unknown_approval",
            GateError::DigestMismatch => "digest_mismatch",
            GateError::TooEarly => "too_early",
            GateError::Denied => "denied",
            GateError::Timeout => "timeout",
            GateError::UiUnavailable => "ui_unavailable",
            GateError::Voided => "voided",
            GateError::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for GateError {}

fn why_to_error(w: DenyWhy) -> GateError {
    match w {
        DenyWhy::User => GateError::Denied,
        DenyWhy::Timeout => GateError::Timeout,
        DenyWhy::UiUnavailable => GateError::UiUnavailable,
        DenyWhy::Voided => GateError::Voided,
        DenyWhy::Cancelled => GateError::Cancelled,
    }
}

/// `evaluate`'s verdict (it never changes any state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evaluation {
    /// May go to `authorize` (or, for a Read op, `complete_read`) without asking.
    Allow,
    /// A `StepGrant` is needed (`request_step_approval`).
    NeedsApproval,
    Deny(DenyReason),
    /// Observe mode: what would be done, at this tier. Nothing is executed.
    WouldDo(Tier),
}

/// How the driver's execution ended (for `record_result`). No OS text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecKind {
    Executed,
    Refused,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecResult {
    pub kind: ExecKind,
    /// Fixed token (a `DriverError` code), or `"ok"`.
    pub code: &'static str,
}

impl ExecResult {
    pub fn executed() -> Self {
        Self {
            kind: ExecKind::Executed,
            code: "ok",
        }
    }
    pub fn refused(code: &'static str) -> Self {
        Self {
            kind: ExecKind::Refused,
            code,
        }
    }
    pub fn failed(code: &'static str) -> Self {
        Self {
            kind: ExecKind::Failed,
            code,
        }
    }
}

/// `ApprovedAction::check_live` failed: the driver must stop before its next input primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revoked;

impl fmt::Display for Revoked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("revoked")
    }
}

impl std::error::Error for Revoked {}

// ---------------------------------------------------------------------------------------------
// Sealed capabilities
// ---------------------------------------------------------------------------------------------

/// Permission to execute exactly one bound action. Minted only by [`PolicyGate::authorize`];
/// sealed (private fields), single use (consumed by value by the driver), no `Clone`, `Copy`,
/// `Default`, serde or `From`. `Debug` prints the digest prefix only.
pub struct ApprovedAction {
    digest: ActionDigest,
    step: BoundStep,
    plan_digest: PlanDigest,
    task: TaskId,
    epoch: u64,
    kill: KillSwitch,
    void_at: u64,
    void: Arc<AtomicU64>,
    clock: Arc<dyn Clock>,
    minted: Duration,
}

impl ApprovedAction {
    /// The only constructor; `pub(crate)` and called only from [`PolicyGate::authorize`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mint(
        digest: ActionDigest,
        step: BoundStep,
        plan_digest: PlanDigest,
        task: TaskId,
        kill: &KillSwitch,
        void: &Arc<AtomicU64>,
        clock: &Arc<dyn Clock>,
        minted: Duration,
        epoch: u64,
        void_at: u64,
    ) -> Self {
        // `epoch` and `void_at` are the values validated under the lock (never re-read here).
        Self {
            digest,
            step,
            plan_digest,
            task,
            epoch,
            kill: kill.clone(),
            void_at,
            void: Arc::clone(void),
            clock: Arc::clone(clock),
            minted,
        }
    }

    pub fn digest(&self) -> ActionDigest {
        self.digest
    }
    pub fn plan_digest(&self) -> PlanDigest {
        self.plan_digest
    }
    pub fn task(&self) -> TaskId {
        self.task
    }
    pub fn step(&self) -> &BoundStep {
        &self.step
    }
    /// The validated action to perform (always present: `authorize` never mints without one).
    pub fn action(&self) -> &Action {
        // `authorize` refuses `NotAnAction` before minting, so this is always `Some`.
        self.step.action().expect("minted with an action")
    }

    /// The driver calls this before EVERY input primitive (each chunk, each click). `Err` when the
    /// kill epoch moved, the gate was voided, or the action is older than 2 s.
    pub fn check_live(&self) -> Result<(), Revoked> {
        if self.kill.epoch() != self.epoch
            || self.void.load(Ordering::SeqCst) != self.void_at
            || self.clock.now().saturating_sub(self.minted) > ACTION_TTL
        {
            return Err(Revoked);
        }
        Ok(())
    }
}

impl fmt::Debug for ApprovedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApprovedAction({})", self.digest.short())
    }
}

/// Authority to run the frozen plan, in order, once each. Sealed; tracks the next step; dies with
/// the task, a kill, `void_all`, or expiry. No `Clone`.
pub struct PlanGrant {
    gate: u64,
    task: u64,
    plan_digest: PlanDigest,
    next_step: u16,
    epoch: u64,
    void_gen: u64,
}

impl PlanGrant {
    pub fn plan_digest(&self) -> PlanDigest {
        self.plan_digest
    }
    pub fn next_step(&self) -> u16 {
        self.next_step
    }
    pub fn task(&self) -> TaskId {
        TaskId(self.task)
    }
}

impl fmt::Debug for PlanGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PlanGrant({}, next {})",
            self.plan_digest, self.next_step
        )
    }
}

/// Authority for exactly one step: its action digest is bound in. Consumed by value by
/// `authorize`. Sealed; no `Clone`.
pub struct StepGrant {
    gate: u64,
    task: u64,
    plan_digest: PlanDigest,
    step: u16,
    action: ActionDigest,
    epoch: u64,
    void_gen: u64,
}

impl StepGrant {
    pub fn action_digest(&self) -> ActionDigest {
        self.action
    }
    pub fn step(&self) -> u16 {
        self.step
    }
}

impl fmt::Debug for StepGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StepGrant(step {}, {})", self.step, self.action)
    }
}

// ---------------------------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Idle,
    Pending(ApprovalId),
    /// `authorize` is between its precheck and its commit (the audit append is in flight). Carries
    /// the full identity of what was validated in phase A; phase C re-validates ALL of it.
    Reserving(Resv),
    /// A minted action the driver has not reported on. Released by `record_result`, or treated
    /// as released once `ACTION_TTL` has passed (the action is dead by then). Ending the task
    /// does NOT release it.
    Outstanding {
        digest: ActionDigest,
        step: u16,
        task: TaskId,
        /// The proposal that minted it: ownership is by proposal id, never by `TaskId` (a
        /// re-proposal may reuse the same `TaskId`).
        proposal: u64,
        at: Duration,
    },
}

/// What `authorize` validated in phase A (task, proposal, plan, step, generations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Resv {
    /// Unique per reservation (so a stale call can never release or commit another's).
    token: u64,
    task: TaskId,
    proposal: u64,
    plan: PlanDigest,
    step: u16,
    void_gen: u64,
    /// Kill epoch of the TASK as validated (task epoch == grant epoch == switch epoch).
    epoch: u64,
    /// When the reservation was taken (gate clock): see [`RESERVE_MAX_AGE`].
    at: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AState {
    Waiting,
    Approved,
    Denied(DenyWhy),
    Voided,
}

enum Payload {
    Plan {
        proposal: u64,
        plan_digest: PlanDigest,
        shown: Vec<Shown>,
    },
    Step {
        plan_digest: PlanDigest,
        step: u16,
        action: ActionDigest,
    },
}

struct Approval {
    kind: ApprovalKind,
    task: TaskId,
    digest: [u8; 32],
    hex: String,
    shown_at: Duration,
    deadline: Duration,
    state: AState,
    void_gen: u64,
    payload: Payload,
}

struct Frozen {
    digest: PlanDigest,
    shown: Vec<Shown>,
    approved_at: Duration,
    last_progress: Duration,
    next_step: u16,
}

struct TaskState {
    task: TaskId,
    proposal: u64,
    origin: TaskOrigin,
    mode: Mode,
    steps: Vec<PlanStep>,
    frozen: Option<Frozen>,
    paused: bool,
    ended: Option<EndReason>,
    /// Kill epoch when the task was proposed.
    epoch: u64,
}

struct LatestObs {
    id: ObservationId,
    at: Duration,
    session: SessionState,
}

struct Inner {
    event_seq: u64,
    audit_seq: u64,
    approval_counter: u64,
    proposal_counter: u64,
    task: Option<TaskState>,
    retired: HashSet<[u8; 32]>,
    approvals: HashMap<ApprovalId, Approval>,
    busy: Busy,
    reservation_counter: u64,
    latest_obs: Option<LatestObs>,
    spent_obs: HashSet<[u8; 24]>,
    mints: VecDeque<Duration>,
    last_mint: Option<Duration>,
}

/// Events and audit records collected under the lock, delivered after it is released.
#[derive(Default)]
struct Out {
    events: Vec<AgentEvent>,
    records: Vec<AuditRecord>,
    kills: Vec<AuditRecord>,
}

impl Out {
    fn is_empty(&self) -> bool {
        self.events.is_empty() && self.records.is_empty() && self.kills.is_empty()
    }
}

impl Inner {
    fn ev(&mut self, out: &mut Out, task: Option<TaskId>, kind: AgentEventKind) {
        self.event_seq += 1;
        out.events.push(AgentEvent {
            seq: self.event_seq,
            task,
            kind,
        });
    }

    fn stamp(&mut self, now: Duration, mut r: AuditRecord, task: Option<TaskId>) -> AuditRecord {
        self.audit_seq += 1;
        r.seq = self.audit_seq;
        r.ts_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        r.task = task.map(|t| t.0);
        r
    }

    fn aud(&mut self, out: &mut Out, now: Duration, task: Option<TaskId>, r: AuditRecord) {
        let r = self.stamp(now, r, task);
        out.records.push(r);
    }

    fn mode(&self) -> Mode {
        match &self.task {
            Some(t) if t.ended.is_none() => t.mode,
            _ => Mode::Observe,
        }
    }
}

fn blank(event: &'static str) -> AuditRecord {
    AuditRecord::blank(event)
}

/// The stricter of two classifications (STRICTER WINS).
fn stricter(a: Classification, b: Classification) -> Classification {
    if b.tier > a.tier {
        return Classification {
            tier: b.tier,
            submit: a.submit || b.submit,
            batchable: a.batchable && b.batchable,
            reason: b.reason,
        };
    }
    Classification {
        tier: a.tier,
        submit: a.submit || b.submit,
        batchable: a.batchable && b.batchable,
        reason: a.reason,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Mint,
    Read,
    Evaluate,
}

enum Effect {
    None,
    End(EndReason),
    Pause,
}

struct Pre {
    cls: Classification,
    /// `Some(changed)` when a single-step approval is needed.
    need: Option<bool>,
    satisfied: bool,
    digest: Option<ActionDigest>,
    plan_digest: PlanDigest,
}

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

pub struct PolicyGate {
    cfg: PolicyConfig,
    policy: PolicyContext,
    kill: KillSwitch,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    events: Mutex<Arc<dyn EventSink>>,
    /// 32 random bytes per gate, memory only. `None` = no OS randomness: the gate is dead.
    nonce: Option<[u8; 32]>,
    /// Per-gate id derived from the nonce: plans, reports and grants carry it, so another gate
    /// refuses them (0 when the gate is dead).
    gate_id: u64,
    /// Set (once, forever) when the inner lock was found poisoned: fail closed.
    poisoned: AtomicBool,
    void: Arc<AtomicU64>,
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl fmt::Debug for PolicyGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PolicyGate({})", self.mode().as_str())
    }
}

impl PolicyGate {
    pub fn new(
        cfg: PolicyConfig,
        kill: KillSwitch,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        let mut nonce = [0u8; 32];
        let nonce = getrandom::fill(&mut nonce).ok().map(|()| nonce);
        let gate_id = nonce.as_ref().map_or(0, gate_id_of);
        let policy = PolicyContext::new(cfg.never_apps.clone());
        let gate = Arc::new(Self {
            cfg,
            policy,
            kill,
            audit,
            clock,
            events: Mutex::new(Arc::new(NullEvents)),
            nonce,
            gate_id,
            poisoned: AtomicBool::new(false),
            void: Arc::new(AtomicU64::new(0)),
            inner: Mutex::new(Inner {
                event_seq: 0,
                audit_seq: 0,
                approval_counter: 0,
                proposal_counter: 0,
                task: None,
                retired: HashSet::new(),
                approvals: HashMap::new(),
                busy: Busy::Idle,
                reservation_counter: 0,
                latest_obs: None,
                spent_obs: HashSet::new(),
                mints: VecDeque::new(),
                last_mint: None,
            }),
            cv: Condvar::new(),
        });
        let weak: Weak<PolicyGate> = Arc::downgrade(&gate);
        gate.kill.on_trip(Arc::new(move |reason| {
            if let Some(g) = weak.upgrade() {
                g.void_all(VoidReason::Killed(reason));
            }
        }));
        gate
    }

    /// Where events go (default: dropped). Call before the first task.
    pub fn set_event_sink(&self, sink: Arc<dyn EventSink>) {
        *self.events.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    pub fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// The audit sink this gate writes to. Read-only: lets the supervisor prove that its own
    /// records go to the SAME append-only log (`audit_mismatch` otherwise).
    pub fn audit_sink(&self) -> Arc<dyn AuditSink> {
        Arc::clone(&self.audit)
    }

    pub fn kill_switch(&self) -> &KillSwitch {
        &self.kill
    }

    /// The Never-app list etc. the gate classifies with (for building a `BindContext`).
    pub fn policy_context(&self) -> &PolicyContext {
        &self.policy
    }

    /// Current mode: Live only while a plan-approved task is running; Observe otherwise.
    pub fn mode(&self) -> Mode {
        let g = self.lock();
        if self.dead() {
            return Mode::Observe;
        }
        g.mode()
    }

    /// Read-only view of what is running (the audit-log guard of S2-13): a live task, and the
    /// busy token. A poisoned gate reports the most busy state (fail closed). Never changes state.
    pub fn activity(&self) -> Activity {
        let now = self.clock.now();
        let g = self.lock();
        if self.dead() {
            return Activity {
                task_live: true,
                busy_state: BusyState::Reserving,
            };
        }
        let live = g.task.as_ref().is_some_and(|t| t.ended.is_none());
        let busy_state = if Self::idle(&g, now) {
            BusyState::Idle
        } else {
            match g.busy {
                Busy::Idle => BusyState::Idle,
                Busy::Pending(_) => BusyState::Pending,
                Busy::Reserving(_) => BusyState::Reserving,
                Busy::Outstanding { .. } => BusyState::Outstanding,
            }
        };
        Activity {
            task_live: live,
            busy_state,
        }
    }

    pub fn current_task(&self) -> Option<TaskId> {
        let g = self.lock();
        if self.dead() {
            return None;
        }
        g.task
            .as_ref()
            .filter(|t| t.ended.is_none())
            .map(|t| t.task)
    }

    /// Takes the gate lock. A poisoned lock (a panic while it was held) FAILS CLOSED: the gate is
    /// marked dead, every grant and action is voided, waiters are woken, and every entry point
    /// then refuses (`Refusal::GatePoisoned`) after calling [`dead`](Self::dead). The recovered
    /// guard is returned only so cleanup (releasing a reservation) can still run.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                self.poison();
                e.into_inner()
            }
        }
    }

    fn poison(&self) {
        if !self.poisoned.swap(true, Ordering::SeqCst) {
            // Every outstanding grant/action fails its void check from now on.
            self.void.fetch_add(1, Ordering::SeqCst);
        }
        self.cv.notify_all();
    }

    /// True once the lock was found poisoned. Call after taking the lock.
    fn dead(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst) || self.inner.is_poisoned()
    }

    /// No busy token is held (an action past `ACTION_TTL` is dead and counts as released).
    fn idle(g: &Inner, now: Duration) -> bool {
        match g.busy {
            Busy::Idle => true,
            Busy::Outstanding { at, .. } => now.saturating_sub(at) > ACTION_TTL,
            // A reservation whose audit append hung past the cap is dead (see `sweep`).
            Busy::Reserving(r) => now.saturating_sub(r.at) > RESERVE_MAX_AGE,
            Busy::Pending(_) => false,
        }
    }

    /// Delivers collected events and audit records. Call with NO lock held.
    fn flush(&self, out: Out) {
        if out.is_empty() {
            return;
        }
        let sink = Arc::clone(&self.events.lock().unwrap_or_else(|e| e.into_inner()));
        for e in &out.events {
            sink.emit(e);
        }
        for r in &out.records {
            self.audit.record(r);
        }
        for r in &out.kills {
            self.audit.killed(r);
        }
    }

    // ----- propose / dry run ------------------------------------------------------------

    /// Validates an untrusted draft (S2-11a), classifies each step provisionally and starts a new
    /// task in Observe mode (a task still running is ended: `superseded`).
    pub fn propose(
        &self,
        task: TaskId,
        draft_json: &str,
        ctx: &PlanContext<'_>,
    ) -> Result<ProposedPlan, PlanError> {
        self.propose_as(task, self.cfg.origin, draft_json, ctx)
    }

    /// As [`propose`](Self::propose) with an explicit origin; the stricter of `origin` and the
    /// configured one applies.
    pub fn propose_as(
        &self,
        task: TaskId,
        origin: TaskOrigin,
        draft_json: &str,
        ctx: &PlanContext<'_>,
    ) -> Result<ProposedPlan, PlanError> {
        let nonce = self.nonce.ok_or(PlanError::GateUnavailable)?;
        let plan = validate(draft_json, ctx)?;
        let origin = self.cfg.origin.stricter(origin);
        let provisional: Vec<Classification> =
            plan.steps().iter().map(|s| self.provisional(s)).collect();
        let now = self.clock.now();
        let digest8 = plan.plan_digest(&nonce, task.0, &[]).short();
        let steps = plan.len();
        let mut out = Out::default();
        let id = {
            let mut g = self.lock();
            if self.dead() {
                return Err(PlanError::GateUnavailable);
            }
            self.end_task(&mut g, EndReason::Superseded, now, &mut out);
            g.proposal_counter += 1;
            let id = g.proposal_counter;
            g.task = Some(TaskState {
                task,
                proposal: id,
                origin,
                mode: Mode::Observe,
                steps: plan.steps().to_vec(),
                frozen: None,
                paused: false,
                ended: None,
                epoch: self.kill.epoch(),
            });
            g.latest_obs = None;
            g.ev(&mut out, Some(task), AgentEventKind::TaskStarted { origin });
            g.ev(
                &mut out,
                Some(task),
                AgentEventKind::PlanProposed {
                    digest8: digest8.clone(),
                    steps: steps as u16,
                },
            );
            let mut r = blank("plan_proposed");
            r.plan_digest8 = Some(digest8);
            r.decision = Some(origin.as_str());
            g.aud(&mut out, now, Some(task), r);
            id
        };
        self.flush(out);
        Ok(ProposedPlan {
            id,
            gate: self.gate_id,
            task,
            origin,
            plan,
            provisional,
        })
    }

    /// Display-only classification: benign environment, unknown target role, so a click is
    /// Consequential and a type is Consequential; labels and text can only raise it.
    fn provisional(&self, step: &PlanStep) -> Classification {
        let mut f = BoundStepFacts::for_step(step)
            .own_process(false)
            .higher_integrity(false)
            .restricted_app(false)
            .denied_app(false)
            .payment_context(false)
            .secure(false)
            .focused_secure(false)
            .session(SessionState::Active);
        if let Some(a) = AppId::parse("app.exe") {
            f = f.app(a);
        }
        if matches!(
            step,
            PlanStep::OpenApp { .. } | PlanStep::FocusWindow { .. }
        ) {
            f = f.target_denied(false);
        }
        if matches!(step, PlanStep::Type { .. }) {
            f = f.editable(true);
        }
        crate::classify::classify(&f, &self.policy)
    }

    /// Binds every step against the CURRENT observation. Nothing is executed, nothing is minted,
    /// nothing is written to the audit log; steps that cannot bind are reported `Unbound`.
    pub fn dry_run(
        &self,
        plan: &ProposedPlan,
        obs: &Observation,
        ctx: &BindContext,
    ) -> DryRunReport {
        let steps: Vec<DryRunStep> = plan
            .steps()
            .iter()
            .enumerate()
            .map(|(i, s)| match bind_step(s, obs, &ctx.for_step(i as u16)) {
                Ok(b) => {
                    let fresh = crate::classify::classify(b.facts(), &self.policy);
                    let cls = stricter(b.classification(), fresh);
                    DryRunStep::Bound(DryBound::of(&b, cls))
                }
                Err(e) => DryRunStep::Unbound(e),
            })
            .collect();
        let mut out = Out::default();
        {
            let mut g = self.lock();
            for (i, s) in steps.iter().enumerate() {
                let tier = match s {
                    DryRunStep::Bound(b) => Some(b.classification().tier),
                    DryRunStep::Unbound(_) => None,
                };
                g.ev(
                    &mut out,
                    Some(plan.task),
                    AgentEventKind::ActionSimulated {
                        step: i as u16,
                        tier,
                    },
                );
            }
        }
        self.flush(out);
        DryRunReport::new(plan.id, self.gate_id, steps)
    }

    /// The caller (core) reports each fresh observation and the preflight result taken with it.
    /// Authorisation requires the step to be bound from the latest noted observation (at most
    /// 60 s old) with an Active session. A spent observation (one that minted an action) cannot
    /// be noted again: one observation authorises at most one action.
    pub fn note_observation(
        &self,
        id: ObservationId,
        session: SessionState,
    ) -> Result<(), GateError> {
        let now = self.clock.now();
        let mut g = self.lock();
        if self.dead() {
            return Err(GateError::Refused(Refusal::GatePoisoned));
        }
        if g.spent_obs.contains(&id.canonical()) {
            return Err(GateError::Refused(Refusal::Stale));
        }
        g.latest_obs = Some(LatestObs {
            id,
            at: now,
            session,
        });
        Ok(())
    }

    // ----- approvals ---------------------------------------------------------------------

    fn next_approval_id(&self, g: &mut Inner) -> Option<ApprovalId> {
        let nonce = self.nonce?;
        loop {
            g.approval_counter += 1;
            let id = approval_id(&nonce, g.approval_counter);
            if !g.approvals.contains_key(&id) {
                return Some(id);
            }
        }
    }

    /// Asks the user to approve the plan together with its dry-run bindings. The digest covers
    /// both; the approval is single use, expires after 120 s (Deny, ends the task) and is
    /// cancelled when the returned handle is dropped.
    pub fn request_plan_approval(
        self: &Arc<Self>,
        plan: &ProposedPlan,
        report: &DryRunReport,
    ) -> Result<PendingPlanApproval, GateError> {
        let nonce = self.nonce.ok_or(GateError::Unavailable)?;
        if plan.gate != self.gate_id
            || report.gate != self.gate_id
            || report.proposal != plan.id
            || report.steps.len() != plan.len()
        {
            return Err(GateError::WrongPlan);
        }
        let now = self.clock.now();
        let mut out = Out::default();
        let mut sweep_out = Out::default();
        let res = {
            let mut g = self.lock();
            if self.dead() {
                return Err(GateError::Refused(Refusal::GatePoisoned));
            }
            self.sweep(&mut g, now, &mut sweep_out);
            let (task_ok, frozen) = match &g.task {
                Some(t) if t.ended.is_none() && t.proposal == plan.id => (true, t.frozen.is_some()),
                _ => (false, false),
            };
            if !task_ok {
                Err(GateError::NoTask)
            } else if frozen {
                Err(GateError::AlreadyApproved)
            } else if !Self::idle(&g, now) {
                Err(GateError::Busy)
            } else {
                let digest = plan
                    .plan
                    .plan_digest(&nonce, plan.task.0, report.canonical_bytes());
                if g.retired.contains(digest.as_bytes()) {
                    Err(GateError::Retired)
                } else {
                    let shown: Vec<Shown> = report
                        .steps
                        .iter()
                        .zip(plan.provisional.iter())
                        .map(|(s, prov)| match s {
                            DryRunStep::Bound(b) => Shown {
                                cap: b.classification,
                                binding: Some(b.clone()),
                            },
                            DryRunStep::Unbound(_) => Shown {
                                // Never shown bound: whatever binds later must be asked again.
                                cap: Classification {
                                    batchable: false,
                                    ..*prov
                                },
                                binding: None,
                            },
                        })
                        .collect();
                    match self.next_approval_id(&mut g) {
                        None => Err(GateError::Unavailable),
                        Some(id) => {
                            let a = Approval {
                                kind: ApprovalKind::Plan,
                                task: plan.task,
                                digest: *digest.as_bytes(),
                                hex: digest.hex(),
                                shown_at: now,
                                deadline: now + PLAN_APPROVAL_TIMEOUT,
                                state: AState::Waiting,
                                void_gen: self.void.load(Ordering::SeqCst),
                                payload: Payload::Plan {
                                    proposal: plan.id,
                                    plan_digest: digest,
                                    shown,
                                },
                            };
                            g.busy = Busy::Pending(id);
                            let hexs = a.hex.clone();
                            g.approvals.insert(id, a);
                            g.ev(
                                &mut out,
                                Some(plan.task),
                                AgentEventKind::ApprovalRequested {
                                    approval_id: id,
                                    digest: hexs,
                                    kind: ApprovalKind::Plan,
                                    timeout_s: PLAN_APPROVAL_TIMEOUT.as_secs(),
                                    changed: false,
                                },
                            );
                            let mut r = blank("approval_requested");
                            r.plan_digest8 = Some(digest.short());
                            r.decision = Some("plan");
                            g.aud(&mut out, now, Some(plan.task), r);
                            Ok((id, digest))
                        }
                    }
                }
            }
        };
        self.flush(sweep_out);
        self.flush(out);
        let (id, digest) = res?;
        Ok(PendingPlanApproval {
            gate: Arc::clone(self),
            id,
            digest,
            done: false,
        })
    }

    /// Asks the user to approve ONE step (a Consequential step the plan grant does not cover, a
    /// submit, or a drifted step). Expires after 60 s (Deny, ends the task).
    pub fn request_step_approval(
        self: &Arc<Self>,
        step: &BoundStep,
    ) -> Result<PendingStepApproval, GateError> {
        let nonce = self.nonce.ok_or(GateError::Unavailable)?;
        let action = step.action().ok_or(GateError::NotAnAction)?;
        let now = self.clock.now();
        let mut out = Out::default();
        let mut sweep_out = Out::default();
        let res = {
            let mut g = self.lock();
            if self.dead() {
                return Err(GateError::Refused(Refusal::GatePoisoned));
            }
            self.sweep(&mut g, now, &mut sweep_out);
            let checked = (|| {
                let t = g.task.as_ref().ok_or(GateError::NoTask)?;
                if t.ended.is_some() {
                    return Err(GateError::NoTask);
                }
                if self.kill.epoch() != t.epoch {
                    return Err(GateError::Refused(Refusal::Killed));
                }
                if t.mode != Mode::Live {
                    return Err(GateError::Refused(Refusal::ObserveMode));
                }
                let fz = t.frozen.as_ref().ok_or(GateError::NoTask)?;
                let idx = usize::from(step.index());
                if step.index() != fz.next_step {
                    return Err(GateError::Refused(Refusal::WrongStepIndex));
                }
                if t.steps.get(idx) != Some(step.plan_step()) {
                    return Err(GateError::OutOfPlan);
                }
                let fresh = crate::classify::classify(step.facts(), &self.policy);
                let cls = stricter(step.classification(), fresh);
                if cls.tier == Tier::Never {
                    return Err(GateError::Refused(Refusal::ActionRefused));
                }
                let shown = fz.shown.get(idx).ok_or(GateError::OutOfPlan)?;
                let prev = idx.checked_sub(1).and_then(|p| t.steps.get(p));
                let changed = drift(shown, step, cls, prev) != Drift::None;
                let d = ActionDigest::new(action_digest(
                    &nonce,
                    t.task.0,
                    fz.digest.as_bytes(),
                    step.index(),
                    action,
                    step.descriptor(),
                ));
                Ok((t.task, fz.digest, cls, changed, d))
            })();
            match checked {
                Err(e) => Err(e),
                Ok((task, plan_digest, cls, changed, d)) => {
                    if !Self::idle(&g, now) {
                        Err(GateError::Busy)
                    } else {
                        match self.next_approval_id(&mut g) {
                            None => Err(GateError::Unavailable),
                            Some(id) => {
                                let kind = if cls.submit {
                                    ApprovalKind::Submit
                                } else {
                                    ApprovalKind::Step
                                };
                                let a = Approval {
                                    kind,
                                    task,
                                    digest: *d.as_bytes(),
                                    hex: d.hex(),
                                    shown_at: now,
                                    deadline: now + STEP_APPROVAL_TIMEOUT,
                                    state: AState::Waiting,
                                    void_gen: self.void.load(Ordering::SeqCst),
                                    payload: Payload::Step {
                                        plan_digest,
                                        step: step.index(),
                                        action: d,
                                    },
                                };
                                g.busy = Busy::Pending(id);
                                let hexs = a.hex.clone();
                                g.approvals.insert(id, a);
                                g.ev(
                                    &mut out,
                                    Some(task),
                                    AgentEventKind::ApprovalRequested {
                                        approval_id: id,
                                        digest: hexs,
                                        kind,
                                        timeout_s: STEP_APPROVAL_TIMEOUT.as_secs(),
                                        changed,
                                    },
                                );
                                let mut r = blank("approval_requested");
                                r.plan_digest8 = Some(plan_digest.short());
                                r.action_digest8 = Some(d.short());
                                r.step = Some(step.index());
                                r.tier = Some(cls.tier.as_str());
                                r.decision = Some(kind.as_str());
                                g.aud(&mut out, now, Some(task), r);
                                Ok((id, d))
                            }
                        }
                    }
                }
            }
        };
        self.flush(sweep_out);
        self.flush(out);
        let (id, digest) = res?;
        Ok(PendingStepApproval {
            gate: Arc::clone(self),
            id,
            digest,
            done: false,
        })
    }

    /// The approval window's answer. `shown_digest_hex` must be the digest that was shown.
    /// Single use: an unknown or already decided id is `UnknownApproval` and changes nothing;
    /// a wrong digest is `DigestMismatch` and the approval stays pending; an APPROVE within
    /// 500 ms of the approval being shown is ignored (`TooEarly`, still pending). A Deny is
    /// always accepted (it is the safe answer).
    pub fn decide(
        &self,
        id: ApprovalId,
        shown_digest_hex: &str,
        d: Decision,
    ) -> Result<(), GateError> {
        let now = self.clock.now();
        let mut out = Out::default();
        let res = {
            let mut g = self.lock();
            if self.dead() {
                return Err(GateError::Refused(Refusal::GatePoisoned));
            }
            self.sweep(&mut g, now, &mut out);
            match g.approvals.get_mut(&id) {
                Some(a) if a.state == AState::Waiting => {
                    if d == Decision::Approve && now.saturating_sub(a.shown_at) < INPUT_GUARD {
                        Err(GateError::TooEarly)
                    } else if !a.hex.eq_ignore_ascii_case(shown_digest_hex) {
                        Err(GateError::DigestMismatch)
                    } else {
                        let (task, kind) = (a.task, a.kind);
                        a.state = match d {
                            Decision::Approve => AState::Approved,
                            Decision::Deny => AState::Denied(DenyWhy::User),
                        };
                        let k = match d {
                            Decision::Approve => AgentEventKind::ApprovalGranted {
                                approval_id: id,
                                kind,
                            },
                            Decision::Deny => AgentEventKind::ApprovalDenied {
                                approval_id: id,
                                why: DenyWhy::User,
                            },
                        };
                        g.ev(&mut out, Some(task), k);
                        let mut r = blank("approval_decided");
                        r.decision = Some(match d {
                            Decision::Approve => "approve",
                            Decision::Deny => "deny",
                        });
                        g.aud(&mut out, now, Some(task), r);
                        if d == Decision::Deny {
                            // Deny ends the task (the step is not skipped).
                            self.end_task(&mut g, EndReason::Denied, now, &mut out);
                        }
                        Ok(())
                    }
                }
                _ => Err(GateError::UnknownApproval),
            }
        };
        self.cv.notify_all();
        self.flush(out);
        res
    }

    /// The approval window really appeared: restart the 500 ms input guard from now (only ever
    /// later, so stricter).
    pub fn mark_shown(&self, id: ApprovalId) {
        let now = self.clock.now();
        let mut g = self.lock();
        if let Some(a) = g.approvals.get_mut(&id) {
            if a.state == AState::Waiting && now > a.shown_at {
                a.shown_at = now;
            }
        }
    }

    /// The panel could not be shown: Deny (`ui_unavailable`), which ends the task.
    pub fn ui_unavailable(&self, id: ApprovalId) {
        let now = self.clock.now();
        let mut out = Out::default();
        {
            let mut g = self.lock();
            self.deny(&mut g, id, DenyWhy::UiUnavailable, now, &mut out);
        }
        self.cv.notify_all();
        self.flush(out);
    }

    /// Marks a waiting approval denied and ends its task. Caller holds the lock.
    fn deny(&self, g: &mut Inner, id: ApprovalId, why: DenyWhy, now: Duration, out: &mut Out) {
        let Some(a) = g.approvals.get_mut(&id) else {
            return;
        };
        if a.state != AState::Waiting {
            return;
        }
        a.state = AState::Denied(why);
        let task = a.task;
        g.ev(
            out,
            Some(task),
            AgentEventKind::ApprovalDenied {
                approval_id: id,
                why,
            },
        );
        let end = if why == DenyWhy::Timeout {
            EndReason::Timeout
        } else {
            EndReason::Denied
        };
        self.end_task(g, end, now, out);
    }

    /// Expires waiting approvals past their deadline (timeout is Deny and ends the task).
    fn sweep(&self, g: &mut Inner, now: Duration, out: &mut Out) {
        // A reservation older than the cap (a hung audit sink) is poisoned: the void generation
        // moves, so its late phase C is refused, and the gate returns to Idle.
        if let Busy::Reserving(r) = g.busy {
            if now.saturating_sub(r.at) > RESERVE_MAX_AGE {
                self.void.fetch_add(1, Ordering::SeqCst);
                g.busy = Busy::Idle;
                let mut rec = blank("reservation_expired");
                rec.plan_digest8 = Some(r.plan.short());
                rec.step = Some(r.step);
                rec.decision = Some("deny");
                rec.result_code = Some("reserve_timeout");
                g.aud(out, now, Some(r.task), rec);
            }
        }
        let due: Vec<ApprovalId> = g
            .approvals
            .iter()
            .filter(|(_, a)| a.state == AState::Waiting && now >= a.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.deny(g, id, DenyWhy::Timeout, now, out);
        }
        if !g.approvals.is_empty() {
            self.cv.notify_all();
        }
    }

    /// Ends the current task: retires its plan digest(s), returns to Observe, voids what is
    /// still pending. Caller holds the lock.
    fn end_task(&self, g: &mut Inner, why: EndReason, now: Duration, out: &mut Out) {
        let Some(t) = g.task.as_mut() else {
            return;
        };
        if t.ended.is_some() {
            return;
        }
        t.ended = Some(why);
        t.mode = Mode::Observe;
        let task = t.task;
        // Every end except a normal finish or an execution failure kills what the task has live
        // (a superseded, denied, expired, refused or timed-out task's `ApprovedAction` dies at
        // once instead of lingering up to `ACTION_TTL`). Completed / ExecFailed are reported by
        // the driver after it is done with its action.
        if !matches!(why, EndReason::Completed | EndReason::ExecFailed) {
            self.void.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(f) = &t.frozen {
            g.retired.insert(*f.digest.as_bytes());
        }
        let mut voided = Vec::new();
        for (id, a) in g.approvals.iter_mut() {
            if a.task != task {
                continue;
            }
            if a.kind == ApprovalKind::Plan {
                g.retired.insert(a.digest);
            }
            if matches!(a.state, AState::Waiting | AState::Approved) {
                if a.state == AState::Waiting {
                    voided.push(*id);
                }
                a.state = AState::Voided;
            }
        }
        for id in voided {
            g.ev(
                out,
                Some(task),
                AgentEventKind::ApprovalDenied {
                    approval_id: id,
                    why: DenyWhy::Voided,
                },
            );
        }
        // An in-flight reservation or outstanding action keeps the busy token (it is released by
        // `record_result`, by `ACTION_TTL`, or by kill/void): ending a task never lets a second
        // action mint beside a live one. Only an approval wait is released here.
        if matches!(g.busy, Busy::Pending(_)) {
            g.busy = Busy::Idle;
        }
        g.latest_obs = None;
        g.ev(
            out,
            Some(task),
            AgentEventKind::TaskFinished {
                outcome: why.as_str(),
            },
        );
        let mut r = blank("task_finished");
        r.result_code = Some(why.as_str());
        g.aud(out, now, Some(task), r);
        self.cv.notify_all();
    }

    /// Kill, observe toggled, settings/deny-list change, quit: every pending approval becomes
    /// Deny (waiters wake with `Voided`), the plan digest is retired for the session (it can never
    /// be granted again), every grant and `ApprovedAction` becomes invalid, the mode is Observe.
    /// The task is terminal; the next task needs a new plan and new approvals.
    pub fn void_all(&self, why: VoidReason) {
        let now = self.clock.now();
        let mut out = Out::default();
        {
            let mut g = self.lock();
            self.void.fetch_add(1, Ordering::SeqCst);
            let task = g.task.as_ref().map(|t| t.task);
            // Retire the digests of pending plan approvals even when no task is running.
            let pending: Vec<[u8; 32]> = g
                .approvals
                .values()
                .filter(|a| a.kind == ApprovalKind::Plan)
                .map(|a| a.digest)
                .collect();
            g.retired.extend(pending);
            let end = match why {
                VoidReason::Killed(_) => EndReason::Killed,
                _ => EndReason::Voided,
            };
            // Anything still waiting (also approvals of an already ended task).
            let waiting: Vec<(ApprovalId, TaskId)> = g
                .approvals
                .iter()
                .filter(|(_, a)| a.state == AState::Waiting)
                .map(|(id, a)| (*id, a.task))
                .collect();
            for (id, t) in waiting {
                g.ev(
                    &mut out,
                    Some(t),
                    AgentEventKind::ApprovalDenied {
                        approval_id: id,
                        why: DenyWhy::Voided,
                    },
                );
            }
            for a in g.approvals.values_mut() {
                if matches!(a.state, AState::Waiting | AState::Approved) {
                    a.state = AState::Voided;
                }
            }
            self.end_task(&mut g, end, now, &mut out);
            // Void/kill: every grant and action is dead (void generation / kill epoch), and a
            // reservation in flight fails its phase-C identity check, so the token is released.
            g.busy = Busy::Idle;
            g.latest_obs = None;
            if let VoidReason::Killed(r) = why {
                g.ev(
                    &mut out,
                    task,
                    AgentEventKind::Killed { reason: r.as_str() },
                );
            }
            let mut rec = blank(if matches!(why, VoidReason::Killed(_)) {
                "killed"
            } else {
                "voided"
            });
            rec.result_code = Some(why.as_str());
            rec.decision = Some("deny");
            let rec = g.stamp(now, rec, task);
            if matches!(why, VoidReason::Killed(_)) {
                out.kills.push(rec);
            } else {
                out.records.push(rec);
            }
        }
        self.cv.notify_all();
        self.flush(out);
    }

    /// "Observe toggled": everything is voided and the mode is Observe.
    pub fn set_observe(&self) {
        self.void_all(VoidReason::ObserveToggled);
    }

    // ----- evaluate / authorize ------------------------------------------------------------

    /// The policy verdict for a bound step. Never changes any state. In Observe mode (no grant)
    /// it is `WouldDo(tier)`. Read ops (`observe`, `find_text`, `answer_from_screen`) are data
    /// only: they never reach the driver, need no `ApprovedAction`, and are allowed in Observe
    /// mode by the caller reading through the firewall.
    pub fn evaluate(&self, step: &BoundStep, grant: Option<&PlanGrant>) -> Evaluation {
        let now = self.clock.now();
        let mut out = Out::default();
        let verdict = {
            let mut g = self.lock();
            if self.dead() {
                return Evaluation::Deny(Refusal::GatePoisoned);
            }
            self.sweep(&mut g, now, &mut out);
            let fresh = crate::classify::classify(step.facts(), &self.policy);
            let tier = stricter(step.classification(), fresh).tier;
            match (g.mode(), grant) {
                (Mode::Live, Some(gr)) => {
                    match self.precheck(&g, step, Some(gr), None, Use::Evaluate, now) {
                        Err((r, _)) => Evaluation::Deny(r),
                        Ok(p) if p.need.is_some() => Evaluation::NeedsApproval,
                        Ok(_) => Evaluation::Allow,
                    }
                }
                _ => Evaluation::WouldDo(tier),
            }
        };
        self.flush(out);
        verdict
    }

    fn action_digest_of(
        &self,
        task: TaskId,
        plan: &PlanDigest,
        step: &BoundStep,
    ) -> Option<ActionDigest> {
        let nonce = self.nonce?;
        let action = step.action()?;
        Some(ActionDigest::new(action_digest(
            &nonce,
            task.0,
            plan.as_bytes(),
            step.index(),
            action,
            step.descriptor(),
        )))
    }

    /// Every pre-mint condition, in order, each with its own code. Pure: effects (ending or
    /// pausing the task) are returned, applied only by the caller that acts on them.
    fn precheck(
        &self,
        g: &Inner,
        step: &BoundStep,
        grant: Option<&PlanGrant>,
        single: Option<&StepGrant>,
        usage: Use,
        now: Duration,
    ) -> Result<Pre, (Refusal, Effect)> {
        let none = |r| (r, Effect::None);
        if self.dead() {
            return Err(none(Refusal::GatePoisoned));
        }
        if grant.is_some_and(|gr| gr.gate != self.gate_id) {
            return Err(none(Refusal::WrongPlan));
        }
        let t = g.task.as_ref().ok_or(none(Refusal::NoTask))?;
        if t.ended.is_some() {
            return Err(none(Refusal::TaskEnded));
        }
        // Kill epoch unchanged since the task started. REDUNDANCY (by design): the kill hook
        // (`void_all`) normally ends the task first, so this and the grant-epoch check below are
        // a second line of defence for a switch tripped without its hook having run (and for a
        // task/grant that disagree). `t.epoch == grant.epoch == switch epoch` holds by
        // construction (`commit_plan`); the unit tests force each to diverge separately.
        let epoch = self.kill.epoch();
        if epoch != t.epoch {
            return Err((Refusal::Killed, Effect::End(EndReason::Killed)));
        }
        // Task live (not observe).
        if t.mode != Mode::Live {
            return Err(none(Refusal::ObserveMode));
        }
        let grant = grant.ok_or(none(Refusal::WrongPlan))?;
        if grant.epoch != epoch {
            return Err((Refusal::Killed, Effect::End(EndReason::Killed)));
        }
        if grant.void_gen != self.void.load(Ordering::SeqCst) {
            return Err(none(Refusal::Voided));
        }
        // Grant digest == the frozen plan. (`grant.task` and `grant.gate` are partly redundant
        // with the digest, which binds the task id and the session nonce; the digest check is
        // the one that separates two proposals under the SAME task id.)
        let fz = t.frozen.as_ref().ok_or(none(Refusal::WrongPlan))?;
        if grant.task != t.task.0 || grant.plan_digest != fz.digest {
            return Err(none(Refusal::WrongPlan));
        }
        if g.retired.contains(fz.digest.as_bytes()) {
            return Err(none(Refusal::Voided));
        }
        // 10 min since approval, 30 s without step progress (a pending approval pauses the
        // stall timer: the user is thinking, the approval has its own timeout).
        let stalled = !matches!(g.busy, Busy::Pending(_))
            && now.saturating_sub(fz.last_progress) >= STALL_TTL;
        if now.saturating_sub(fz.approved_at) >= PLAN_GRANT_TTL || stalled {
            return Err((Refusal::Expired, Effect::End(EndReason::Expired)));
        }
        if t.paused {
            return Err(none(Refusal::Paused));
        }
        // Step index is the next one (never reused) and the abstract op is the frozen one.
        if step.index() != grant.next_step || grant.next_step != fz.next_step {
            return Err(none(Refusal::WrongStepIndex));
        }
        let idx = usize::from(step.index());
        let frozen_step = t.steps.get(idx).ok_or(none(Refusal::WrongStepIndex))?;
        if step.plan_step() != frozen_step {
            return Err(none(Refusal::OutOfPlan));
        }
        // Fresh facts.
        let lo = g.latest_obs.as_ref().ok_or(none(Refusal::Stale))?;
        if lo.id != step.observation()
            || now.saturating_sub(lo.at) > OBSERVATION_MAX_AGE
            || now.saturating_sub(step.bound_at()) > OBSERVATION_MAX_AGE
        {
            return Err(none(Refusal::Stale));
        }
        if usage != Use::Read && lo.session != SessionState::Active {
            return Err(none(Refusal::SessionNotActive));
        }
        // Classification recomputed NOW with the fresh session; the stricter of bind-time and now.
        let facts = step.facts().clone().session(lo.session.clone());
        let cls = stricter(
            step.classification(),
            crate::classify::classify(&facts, &self.policy),
        );
        if cls.tier == Tier::Never {
            return Err((Refusal::ActionRefused, Effect::End(EndReason::Refused)));
        }
        match usage {
            Use::Mint if step.action().is_none() => return Err(none(Refusal::NotAnAction)),
            Use::Evaluate if step.action().is_none() && cls.tier > Tier::Read => {
                return Err(none(Refusal::NotAnAction))
            }
            Use::Read if step.action().is_some() || cls.tier != Tier::Read => {
                return Err(none(Refusal::NotARead))
            }
            _ => {}
        }
        // Unattended origins (D-006).
        match t.origin {
            TaskOrigin::Interactive => {}
            TaskOrigin::Scheduled if cls.tier > Tier::Read => {
                return Err((Refusal::ScheduledReadOnly, Effect::Pause))
            }
            TaskOrigin::Background if cls.tier >= Tier::Consequential => {
                return Err((Refusal::UnattendedConsequential, Effect::Pause))
            }
            _ => {}
        }
        // Drift and approval need.
        let shown = fz.shown.get(idx).ok_or(none(Refusal::OutOfPlan))?;
        let prev = idx.checked_sub(1).and_then(|p| t.steps.get(p));
        let drifted = drift(shown, step, cls, prev) != Drift::None;
        let must = cls.tier == Tier::Consequential && (!cls.batchable || cls.submit);
        let need = if drifted {
            Some(true)
        } else if must {
            Some(false)
        } else {
            None
        };
        let digest = self.action_digest_of(t.task, &fz.digest, step);
        let mut satisfied = false;
        if let Some(sg) = single {
            let ok = digest.is_some_and(|d| sg.action == d)
                && sg.gate == self.gate_id
                && sg.task == t.task.0
                && sg.plan_digest == fz.digest
                && sg.step == step.index()
                && sg.epoch == epoch
                && sg.void_gen == self.void.load(Ordering::SeqCst);
            if !ok {
                return Err(none(Refusal::StepGrantMismatch));
            }
            satisfied = true;
        }
        if usage == Use::Mint {
            if let Some(changed) = need {
                if !satisfied {
                    return Err(none(if changed {
                        Refusal::DriftPause
                    } else {
                        Refusal::StepApprovalRequired
                    }));
                }
            }
            // Single busy token.
            if !Self::idle(g, now) {
                return Err(none(Refusal::Busy));
            }
            // Rate floor.
            if g.last_mint
                .is_some_and(|l| now.saturating_sub(l) < MIN_MINT_GAP)
                || g.mints
                    .iter()
                    .filter(|m| now.saturating_sub(**m) < Duration::from_secs(60))
                    .count()
                    >= MAX_MINTS_PER_MINUTE
            {
                return Err(none(Refusal::RateLimited));
            }
        }
        Ok(Pre {
            cls,
            need,
            satisfied,
            digest,
            plan_digest: fz.digest,
        })
    }

    /// Records and applies a refusal. Caller holds the lock.
    fn apply_refusal(
        &self,
        g: &mut Inner,
        step: &BoundStep,
        r: Refusal,
        effect: Effect,
        now: Duration,
        out: &mut Out,
    ) {
        let task = g.task.as_ref().map(|t| t.task);
        let mut rec = blank("refused");
        rec.step = Some(step.index());
        rec.op = Some(step.plan_step().op_name());
        rec.tier = Some(step.classification().tier.as_str());
        rec.decision = Some("deny");
        rec.result_code = Some(r.code());
        g.aud(out, now, task, rec);
        if !r.is_approval_request() {
            g.ev(
                out,
                task,
                AgentEventKind::ActionRefused {
                    step: step.index(),
                    reason: r.code(),
                },
            );
        }
        match effect {
            Effect::None => {}
            Effect::End(why) => self.end_task(g, why, now, out),
            Effect::Pause => {
                if let Some(t) = g.task.as_mut() {
                    t.paused = true;
                }
                g.ev(out, task, AgentEventKind::TaskPaused { reason: r.code() });
            }
        }
    }

    /// Mints an [`ApprovedAction`] for `step` if EVERY condition holds (ADR 0003 section 6):
    /// task live; grant digest equals the frozen plan; step index is the grant's next step and the
    /// op equals the frozen one; fresh observation and Active session; classification now is not
    /// above what was shown and not Never; no drift (else a single-step approval is needed);
    /// a `StepGrant` for exactly this action digest when Consequential and not batchable;
    /// kill epoch unchanged; the audit INTENT append succeeded; nothing else pending or
    /// outstanding; rate floor. Never mints in Observe mode.
    pub fn authorize(
        &self,
        step: BoundStep,
        grant: &mut PlanGrant,
        single: Option<StepGrant>,
    ) -> Result<ApprovedAction, Refusal> {
        let now = self.clock.now();
        let mut out = Out::default();
        // Phase A: check everything, reserve the busy token with the full identity validated.
        let phase_a = {
            let mut g = self.lock();
            self.sweep(&mut g, now, &mut out);
            match self.precheck(&g, &step, Some(grant), single.as_ref(), Use::Mint, now) {
                Err((r, eff)) => {
                    if r != Refusal::GatePoisoned {
                        self.apply_refusal(&mut g, &step, r, eff, now, &mut out);
                    }
                    Err(r)
                }
                Ok(pre) => {
                    let ident = g.task.as_ref().map(|t| (t.task, t.proposal, t.epoch));
                    match (pre.digest, ident) {
                        (Some(d), Some((task, proposal, epoch))) => {
                            g.reservation_counter += 1;
                            let resv = Resv {
                                token: g.reservation_counter,
                                task,
                                proposal,
                                plan: pre.plan_digest,
                                step: step.index(),
                                void_gen: self.void.load(Ordering::SeqCst),
                                epoch,
                                at: now,
                            };
                            g.busy = Busy::Reserving(resv);
                            let rec = self.intent_record(&mut g, &step, d, &pre, task, now);
                            Ok((d, rec, resv))
                        }
                        (None, _) => Err(Refusal::NotAnAction),
                        (_, None) => Err(Refusal::NoTask),
                    }
                }
            }
        };
        self.cv.notify_all();
        // From here until phase C ends, an unwind (a panicking sink) releases the reservation.
        let mut guard = match &phase_a {
            Ok((_, _, resv)) => Some(ReserveGuard {
                gate: self,
                resv: *resv,
            }),
            Err(_) => None,
        };
        self.flush(out);
        let (digest, record, resv) = phase_a?;

        // Phase B: write-ahead intent, no lock held.
        let appended = self.audit.intent(&record);

        // Phase C: commit, re-validating EVERYTHING phase A validated (a superseded, ended,
        // killed or voided task, or another reservation, must never mint).
        let mut out = Out::default();
        let result = {
            let mut g = self.lock();
            let refuse = |g: &mut Inner, out: &mut Out, r: Refusal, intent_written: bool| {
                if g.busy == Busy::Reserving(resv) {
                    g.busy = Busy::Idle;
                }
                if intent_written {
                    // The write-ahead record exists: close it with a content-free follow-up.
                    let mut rec = blank("refused_after_intent");
                    rec.plan_digest8 = Some(resv.plan.short());
                    rec.action_digest8 = Some(digest.short());
                    rec.step = Some(resv.step);
                    rec.decision = Some("deny");
                    rec.result_code = Some(r.code());
                    g.aud(out, now, Some(resv.task), rec);
                } else {
                    let mut rec = blank("refused");
                    rec.step = Some(resv.step);
                    rec.result_code = Some(r.code());
                    g.aud(out, now, Some(resv.task), rec);
                }
                g.ev(
                    out,
                    Some(resv.task),
                    AgentEventKind::ActionRefused {
                        step: resv.step,
                        reason: r.code(),
                    },
                );
                Err(r)
            };
            if self.dead() {
                // Fail closed; the poisoned lock is not trusted with any more bookkeeping.
                Err(Refusal::GatePoisoned)
            } else if appended.is_err() {
                refuse(&mut g, &mut out, Refusal::AuditFailed, false)
            } else {
                let kill_epoch = self.kill.epoch();
                let too_old = self.clock.now().saturating_sub(resv.at) > RESERVE_MAX_AGE;
                let same = !too_old
                    && g.busy == Busy::Reserving(resv)
                    && g.task.as_ref().is_some_and(|t| {
                        t.ended.is_none()
                            && t.mode == Mode::Live
                            && t.task == resv.task
                            && t.proposal == resv.proposal
                            && t.epoch == resv.epoch
                            && t.frozen
                                .as_ref()
                                .is_some_and(|f| f.digest == resv.plan && f.next_step == resv.step)
                    });
                if kill_epoch != resv.epoch {
                    refuse(&mut g, &mut out, Refusal::Killed, true)
                } else if !same || self.void.load(Ordering::SeqCst) != resv.void_gen {
                    refuse(&mut g, &mut out, Refusal::Voided, true)
                } else {
                    grant.next_step = resv.step + 1;
                    if let Some(f) = g.task.as_mut().and_then(|t| t.frozen.as_mut()) {
                        f.next_step = resv.step + 1;
                        f.last_progress = now;
                    }
                    g.mints.push_back(now);
                    while g
                        .mints
                        .front()
                        .is_some_and(|m| now.saturating_sub(*m) >= Duration::from_secs(60))
                    {
                        g.mints.pop_front();
                    }
                    g.last_mint = Some(now);
                    g.spent_obs.insert(step.observation().canonical());
                    g.latest_obs = None;
                    g.busy = Busy::Outstanding {
                        digest,
                        step: resv.step,
                        task: resv.task,
                        proposal: resv.proposal,
                        at: now,
                    };
                    Ok(ApprovedAction::mint(
                        digest,
                        step,
                        resv.plan,
                        resv.task,
                        &self.kill,
                        &self.void,
                        &self.clock,
                        now,
                        resv.epoch,
                        resv.void_gen,
                    ))
                }
            }
        };
        // The reservation has been committed or released under the lock.
        if let Some(gd) = guard.take() {
            std::mem::forget(gd);
        }
        self.cv.notify_all();
        self.flush(out);
        result
    }

    /// The write-ahead audit record for a mint. Never contains typed text, its hash, or an
    /// unredacted label that overlaps the text.
    fn intent_record(
        &self,
        g: &mut Inner,
        step: &BoundStep,
        digest: ActionDigest,
        pre: &Pre,
        task: TaskId,
        now: Duration,
    ) -> AuditRecord {
        let mut r = blank("intent");
        r.plan_digest8 = Some(pre.plan_digest.short());
        r.action_digest8 = Some(digest.short());
        r.step = Some(step.index());
        r.tier = Some(pre.cls.tier.as_str());
        r.decision = Some(if pre.satisfied {
            "allow_step_grant"
        } else {
            "allow_plan_grant"
        });
        if let Some(a) = step.action() {
            r.op = Some(a.op_name());
            r.text_len = a.text_len();
        }
        if let Some(d) = step.descriptor() {
            r.target_role = Some(crate::report::role_str(d.role));
            r.app_id = Some(d.app_id.to_string());
            let typed = match step.action().map(Action::kind) {
                Some(ActionKind::TypeText { text, .. } | ActionKind::SetText { text, .. }) => {
                    Some(text.as_str())
                }
                _ => None,
            };
            let sensitive = d.sensitive_label || step.payment_context();
            r.target_label = Some(audit_label(
                d.label.as_deref().unwrap_or(""),
                typed,
                sensitive,
            ));
        } else if let Some(app) = step.facts().app_id.as_ref() {
            r.app_id = Some(app.to_string());
        }
        g.stamp(now, r, Some(task))
    }

    /// The driver reports how `execute` ended. Clears the busy token. Success counts as step
    /// progress (and ends the task after the last step); a refused or failed execution ends the
    /// task. An unknown digest is ignored.
    pub fn record_result(&self, digest: ActionDigest, r: &ExecResult) {
        let now = self.clock.now();
        let mut out = Out::default();
        {
            let mut g = self.lock();
            self.sweep(&mut g, now, &mut out);
            let matched = match g.busy {
                Busy::Outstanding {
                    digest: d,
                    step,
                    task,
                    proposal,
                    ..
                } if d == digest => Some((d, step, task, proposal)),
                _ => None,
            };
            let Some((d, step, owner, owner_proposal)) = matched else {
                // Unmatched or late (after a kill, a TTL release or a different action): nothing
                // changes, but the report is recorded, content-free.
                let mut rec = blank("late_result");
                rec.action_digest8 = Some(digest.short());
                rec.result_code = Some(r.code);
                g.aud(&mut out, now, None, rec);
                drop(g);
                self.flush(out);
                return;
            };
            g.busy = Busy::Idle;
            // The result belongs to the task that minted the action, which may have been
            // superseded since: it never touches (or ends) another task.
            let task = Some(owner);
            let current = g
                .task
                .as_ref()
                .is_some_and(|t| t.proposal == owner_proposal);
            let mut rec = blank("result");
            rec.action_digest8 = Some(d.short());
            rec.step = Some(step);
            rec.result_code = Some(r.code);
            g.aud(&mut out, now, task, rec);
            match r.kind {
                ExecKind::Executed => {
                    g.ev(
                        &mut out,
                        task,
                        AgentEventKind::ActionExecuted {
                            step,
                            digest8: d.short(),
                            result: r.code,
                        },
                    );
                    let mut done = false;
                    if let Some(t) = g.task.as_mut().filter(|t| t.proposal == owner_proposal) {
                        if let Some(f) = t.frozen.as_mut() {
                            f.last_progress = now;
                            done = usize::from(f.next_step) >= t.steps.len();
                        }
                    }
                    if done {
                        self.end_task(&mut g, EndReason::Completed, now, &mut out);
                    }
                }
                ExecKind::Refused | ExecKind::Failed => {
                    g.ev(
                        &mut out,
                        task,
                        AgentEventKind::ActionRefused {
                            step,
                            reason: r.code,
                        },
                    );
                    if current {
                        self.end_task(&mut g, EndReason::ExecFailed, now, &mut out);
                    }
                }
            }
        }
        self.cv.notify_all();
        self.flush(out);
    }

    /// Completes a Read step (`observe`, `find_text`, `answer_from_screen`): the same conditions
    /// as `authorize` except the mint, the step approval, busy and rate. Advances the grant.
    pub fn complete_read(&self, step: &BoundStep, grant: &mut PlanGrant) -> Result<(), Refusal> {
        let now = self.clock.now();
        let mut out = Out::default();
        let res = {
            let mut g = self.lock();
            self.sweep(&mut g, now, &mut out);
            match self.precheck(&g, step, Some(grant), None, Use::Read, now) {
                Err((Refusal::GatePoisoned, _)) => Err(Refusal::GatePoisoned),
                Err((r, eff)) => {
                    self.apply_refusal(&mut g, step, r, eff, now, &mut out);
                    Err(r)
                }
                Ok(_) => {
                    grant.next_step += 1;
                    self.advance(&mut g, grant.next_step, now, &mut out);
                    let task = g.task.as_ref().map(|t| t.task);
                    let mut rec = blank("read");
                    rec.step = Some(step.index());
                    rec.op = Some(step.plan_step().op_name());
                    rec.tier = Some(Tier::Read.as_str());
                    g.aud(&mut out, now, task, rec);
                    Ok(())
                }
            }
        };
        self.cv.notify_all();
        self.flush(out);
        res
    }

    /// Steps past a step that was refused (Never) at plan time: "shown as refused, never executed,
    /// they don't stop the plan". Only valid for the grant's next step.
    pub fn skip_refused(&self, index: u16, grant: &mut PlanGrant) -> Result<(), Refusal> {
        let now = self.clock.now();
        let mut out = Out::default();
        let res = {
            let mut g = self.lock();
            self.sweep(&mut g, now, &mut out);
            let checked = (|| {
                if self.dead() {
                    return Err(Refusal::GatePoisoned);
                }
                if grant.gate != self.gate_id {
                    return Err(Refusal::WrongPlan);
                }
                let t = g.task.as_ref().ok_or(Refusal::NoTask)?;
                if t.ended.is_some() {
                    return Err(Refusal::TaskEnded);
                }
                if self.kill.epoch() != t.epoch || grant.epoch != t.epoch {
                    return Err(Refusal::Killed);
                }
                if t.mode != Mode::Live {
                    return Err(Refusal::ObserveMode);
                }
                if grant.void_gen != self.void.load(Ordering::SeqCst) {
                    return Err(Refusal::Voided);
                }
                let fz = t.frozen.as_ref().ok_or(Refusal::WrongPlan)?;
                if grant.task != t.task.0 || grant.plan_digest != fz.digest {
                    return Err(Refusal::WrongPlan);
                }
                if now.saturating_sub(fz.approved_at) >= PLAN_GRANT_TTL
                    || now.saturating_sub(fz.last_progress) >= STALL_TTL
                {
                    return Err(Refusal::Expired);
                }
                if index != grant.next_step || grant.next_step != fz.next_step {
                    return Err(Refusal::WrongStepIndex);
                }
                let shown = fz.shown.get(usize::from(index)).ok_or(Refusal::OutOfPlan)?;
                if shown.cap.tier != Tier::Never {
                    return Err(Refusal::NotRefused);
                }
                Ok(())
            })();
            match checked {
                Err(Refusal::Expired) => {
                    self.end_task(&mut g, EndReason::Expired, now, &mut out);
                    Err(Refusal::Expired)
                }
                Err(r) => Err(r),
                Ok(()) => {
                    grant.next_step += 1;
                    self.advance(&mut g, grant.next_step, now, &mut out);
                    let task = g.task.as_ref().map(|t| t.task);
                    g.ev(
                        &mut out,
                        task,
                        AgentEventKind::ActionRefused {
                            step: index,
                            reason: "refused_at_plan_time",
                        },
                    );
                    Ok(())
                }
            }
        };
        self.cv.notify_all();
        self.flush(out);
        res
    }

    /// Records progress to `next` and ends the task after the last step. Caller holds the lock.
    fn advance(&self, g: &mut Inner, next: u16, now: Duration, out: &mut Out) {
        let mut done = false;
        if let Some(t) = g.task.as_mut() {
            let len = t.steps.len();
            if let Some(f) = t.frozen.as_mut() {
                f.next_step = next;
                f.last_progress = now;
                done = usize::from(next) >= len && len > 0;
            }
        }
        if done {
            self.end_task(g, EndReason::Completed, now, out);
        }
    }

    // ----- waiting ---------------------------------------------------------------------

    fn wait_plan(&self, id: ApprovalId) -> Result<PlanGrant, GateError> {
        loop {
            let mut out = Out::default();
            let mut g = self.lock();
            if self.dead() {
                drop(g);
                return Err(GateError::Voided);
            }
            let now = self.clock.now();
            self.sweep(&mut g, now, &mut out);
            let state = g.approvals.get(&id).map(|a| (a.state, a.deadline));
            match state {
                None => {
                    drop(g);
                    self.flush(out);
                    return Err(GateError::UnknownApproval);
                }
                Some((AState::Waiting, deadline)) => {
                    if out.is_empty() {
                        let slice = self.clock.poll_slice(deadline.saturating_sub(now));
                        // A poisoned wait is noticed by the `dead()` check on the next turn.
                        match self.cv.wait_timeout(g, slice) {
                            Ok((g2, _)) => drop(g2),
                            Err(e) => {
                                self.poison();
                                drop(e.into_inner());
                            }
                        }
                    } else {
                        drop(g);
                        self.flush(out);
                    }
                }
                Some((AState::Denied(w), _)) => {
                    g.approvals.remove(&id);
                    if g.busy == Busy::Pending(id) {
                        g.busy = Busy::Idle;
                    }
                    drop(g);
                    self.flush(out);
                    return Err(why_to_error(w));
                }
                Some((AState::Voided, _)) => {
                    g.approvals.remove(&id);
                    if g.busy == Busy::Pending(id) {
                        g.busy = Busy::Idle;
                    }
                    drop(g);
                    self.flush(out);
                    return Err(GateError::Voided);
                }
                Some((AState::Approved, _)) => {
                    let res = self.commit_plan(&mut g, id, now, &mut out);
                    drop(g);
                    self.cv.notify_all();
                    self.flush(out);
                    return res;
                }
            }
        }
    }

    /// An approved plan approval becomes a `PlanGrant`: the task goes Live and the plan freezes.
    fn commit_plan(
        &self,
        g: &mut Inner,
        id: ApprovalId,
        now: Duration,
        out: &mut Out,
    ) -> Result<PlanGrant, GateError> {
        let Some(a) = g.approvals.remove(&id) else {
            return Err(GateError::UnknownApproval);
        };
        if g.busy == Busy::Pending(id) {
            g.busy = Busy::Idle;
        }
        let Payload::Plan {
            proposal,
            plan_digest,
            shown,
        } = a.payload
        else {
            return Err(GateError::UnknownApproval);
        };
        let valid = a.void_gen == self.void.load(Ordering::SeqCst)
            && !g.retired.contains(plan_digest.as_bytes())
            && g.task.as_ref().is_some_and(|t| {
                t.ended.is_none()
                    && t.proposal == proposal
                    && t.frozen.is_none()
                    && t.epoch == self.kill.epoch()
            });
        if !valid {
            return Err(GateError::Voided);
        }
        let epoch = self.kill.epoch();
        let void_gen = self.void.load(Ordering::SeqCst);
        let task = a.task;
        if let Some(t) = g.task.as_mut() {
            t.mode = Mode::Live;
            t.frozen = Some(Frozen {
                digest: plan_digest,
                shown,
                approved_at: now,
                last_progress: now,
                next_step: 0,
            });
        }
        let mut rec = blank("mode");
        rec.plan_digest8 = Some(plan_digest.short());
        rec.decision = Some(Mode::Live.as_str());
        g.aud(out, now, Some(task), rec);
        Ok(PlanGrant {
            gate: self.gate_id,
            task: task.0,
            plan_digest,
            next_step: 0,
            epoch,
            void_gen,
        })
    }

    fn wait_step(&self, id: ApprovalId) -> Result<StepGrant, GateError> {
        loop {
            let mut out = Out::default();
            let mut g = self.lock();
            if self.dead() {
                drop(g);
                return Err(GateError::Voided);
            }
            let now = self.clock.now();
            self.sweep(&mut g, now, &mut out);
            let state = g.approvals.get(&id).map(|a| (a.state, a.deadline));
            match state {
                None => {
                    drop(g);
                    self.flush(out);
                    return Err(GateError::UnknownApproval);
                }
                Some((AState::Waiting, deadline)) => {
                    if out.is_empty() {
                        let slice = self.clock.poll_slice(deadline.saturating_sub(now));
                        // A poisoned wait is noticed by the `dead()` check on the next turn.
                        match self.cv.wait_timeout(g, slice) {
                            Ok((g2, _)) => drop(g2),
                            Err(e) => {
                                self.poison();
                                drop(e.into_inner());
                            }
                        }
                    } else {
                        drop(g);
                        self.flush(out);
                    }
                }
                Some((AState::Denied(w), _)) => {
                    g.approvals.remove(&id);
                    if g.busy == Busy::Pending(id) {
                        g.busy = Busy::Idle;
                    }
                    drop(g);
                    self.flush(out);
                    return Err(why_to_error(w));
                }
                Some((AState::Voided, _)) => {
                    g.approvals.remove(&id);
                    if g.busy == Busy::Pending(id) {
                        g.busy = Busy::Idle;
                    }
                    drop(g);
                    self.flush(out);
                    return Err(GateError::Voided);
                }
                Some((AState::Approved, _)) => {
                    let res = self.commit_step(&mut g, id, now);
                    drop(g);
                    self.cv.notify_all();
                    self.flush(out);
                    return res;
                }
            }
        }
    }

    fn commit_step(
        &self,
        g: &mut Inner,
        id: ApprovalId,
        now: Duration,
    ) -> Result<StepGrant, GateError> {
        let Some(a) = g.approvals.remove(&id) else {
            return Err(GateError::UnknownApproval);
        };
        if g.busy == Busy::Pending(id) {
            g.busy = Busy::Idle;
        }
        let Payload::Step {
            plan_digest,
            step,
            action,
        } = a.payload
        else {
            return Err(GateError::UnknownApproval);
        };
        let valid = a.void_gen == self.void.load(Ordering::SeqCst)
            && g.task.as_ref().is_some_and(|t| {
                t.ended.is_none()
                    && t.mode == Mode::Live
                    && t.epoch == self.kill.epoch()
                    && t.frozen
                        .as_ref()
                        .is_some_and(|f| f.digest == plan_digest && f.next_step == step)
            });
        if !valid {
            return Err(GateError::Voided);
        }
        if let Some(f) = g.task.as_mut().and_then(|t| t.frozen.as_mut()) {
            f.last_progress = now;
        }
        Ok(StepGrant {
            gate: self.gate_id,
            task: a.task.0,
            plan_digest,
            step,
            action,
            epoch: self.kill.epoch(),
            void_gen: self.void.load(Ordering::SeqCst),
        })
    }

    /// Drop of a pending handle: the approval is cancelled and the busy token released.
    fn cancel(&self, id: ApprovalId) {
        let now = self.clock.now();
        let mut out = Out::default();
        {
            let mut g = self.lock();
            if let Some(a) = g.approvals.remove(&id) {
                if g.busy == Busy::Pending(id) {
                    g.busy = Busy::Idle;
                }
                if a.state == AState::Waiting {
                    g.ev(
                        &mut out,
                        Some(a.task),
                        AgentEventKind::ApprovalDenied {
                            approval_id: id,
                            why: DenyWhy::Cancelled,
                        },
                    );
                    let mut rec = blank("approval_cancelled");
                    rec.decision = Some("cancel");
                    g.aud(&mut out, now, Some(a.task), rec);
                }
            }
        }
        self.cv.notify_all();
        self.flush(out);
    }
}

/// Releases an `authorize` reservation if the call unwinds (a panicking audit sink or event
/// sink) so the gate is not silently wedged. Forgotten once phase C settled the reservation.
struct ReserveGuard<'a> {
    gate: &'a PolicyGate,
    resv: Resv,
}

impl Drop for ReserveGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.gate.lock();
        if g.busy == Busy::Reserving(self.resv) {
            g.busy = Busy::Idle;
        }
        drop(g);
        self.gate.cv.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------
// Pending approvals
// ---------------------------------------------------------------------------------------------

/// A plan approval waiting for the user. Dropping it cancels the approval.
pub struct PendingPlanApproval {
    gate: Arc<PolicyGate>,
    id: ApprovalId,
    digest: PlanDigest,
    done: bool,
}

impl PendingPlanApproval {
    pub fn id(&self) -> ApprovalId {
        self.id
    }
    pub fn digest(&self) -> PlanDigest {
        self.digest
    }
    /// The full digest the approval window must echo back to `decide`.
    pub fn digest_hex(&self) -> String {
        self.digest.hex()
    }
    pub fn timeout(&self) -> Duration {
        PLAN_APPROVAL_TIMEOUT
    }

    /// Blocks until decided, timed out (120 s: Deny, ends the task) or voided. Call from a
    /// blocking thread (`spawn_blocking`), never from async code. All time (deadlines, sweeps,
    /// approval and progress stamps) comes from the gate's own clock; the caller cannot supply
    /// one. On approval the task goes Live, bound to this plan digest.
    pub fn wait(mut self) -> Result<PlanGrant, GateError> {
        self.done = true;
        self.gate.wait_plan(self.id)
    }
}

impl Drop for PendingPlanApproval {
    fn drop(&mut self) {
        if !self.done {
            self.gate.cancel(self.id);
        }
    }
}

impl fmt::Debug for PendingPlanApproval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PendingPlanApproval({:?}, {})", self.id, self.digest)
    }
}

/// A single-step approval waiting for the user. Dropping it cancels the approval.
pub struct PendingStepApproval {
    gate: Arc<PolicyGate>,
    id: ApprovalId,
    digest: ActionDigest,
    done: bool,
}

impl PendingStepApproval {
    pub fn id(&self) -> ApprovalId {
        self.id
    }
    pub fn digest(&self) -> ActionDigest {
        self.digest
    }
    pub fn digest_hex(&self) -> String {
        self.digest.hex()
    }
    pub fn timeout(&self) -> Duration {
        STEP_APPROVAL_TIMEOUT
    }

    /// As [`PendingPlanApproval::wait`] with a 60 s timeout; the result is a `StepGrant` for
    /// exactly this step's action digest.
    pub fn wait(mut self) -> Result<StepGrant, GateError> {
        self.done = true;
        self.gate.wait_step(self.id)
    }
}

impl Drop for PendingStepApproval {
    fn drop(&mut self) {
        if !self.done {
            self.gate.cancel(self.id);
        }
    }
}

impl fmt::Debug for PendingStepApproval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PendingStepApproval({:?}, {})", self.id, self.digest)
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
