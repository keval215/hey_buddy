//! hey_buddy policy (ADR 0003 sections 4-9, 11, 13): the strict `plan/v1` model and its
//! re-validation, provenance, the sealed [`ValidatedPlan`], the tier table as the pure function
//! [`classify`], binding of plan steps to a firewall [`Observation`](firewall::Observation)
//! ([`bind_step`]), and the [`PolicyGate`]: the ONLY minter of the sealed [`ApprovedAction`], with
//! plan and step grants, digest-bound single-use approvals, observe mode, the [`KillSwitch`], the
//! audit-intent interface and the event types. See `docs/policy.md`.
//!
//! Pure and synchronous: depends only on `action`, `firewall`, `getrandom` (session nonce),
//! serde/serde_json (to parse the untrusted draft), sha2 and thiserror. Time and waiting go
//! through the injectable [`Clock`] and std `Mutex`/`Condvar` (blocking `wait`, called from
//! `spawn_blocking`). No tokio, llm, capture, voice, driver or OS crates (CI checks).
//! TODO(S2-15): add the matching CI rule that the `agent` crate does not depend on `policy`,
//! `driver`, `capture`, `firewall` or `llm` (`cargo tree -p agent`).
//!
//! # A `ValidatedPlan` cannot be forged
//! Stable rustdoc does not enforce the `EXXXX` in `compile_fail,EXXXX`; the codes below (and in
//! `gate.rs`) were verified by hand with `rustc` and are re-checked by
//! `tools/check-doctest-codes.sh`.
//!
//! ```compile_fail,E0451
//! // Private fields: no struct literal from outside the crate.
//! fn f() -> policy::ValidatedPlan { policy::ValidatedPlan { steps: todo!(), canonical: todo!() } }
//! ```
//! ```compile_fail,E0624
//! // The constructor is pub(crate).
//! fn f() -> policy::ValidatedPlan { policy::ValidatedPlan::from_steps(todo!()) }
//! ```
//! ```compile_fail,E0277
//! // No Default.
//! fn needs_default<T: Default>() {}
//! needs_default::<policy::ValidatedPlan>();
//! ```
//! ```compile_fail,E0599
//! // No Clone (share with `into_shared`).
//! fn f(p: policy::ValidatedPlan) { let _ = p.clone(); }
//! ```
//! ```compile_fail,E0277
//! // No From<String>.
//! fn needs_from<T: From<String>>() {}
//! needs_from::<policy::ValidatedPlan>();
//! ```

mod audit;
mod bind;
mod catalogue;
mod chord;
mod classify;
mod clock;
mod config;
mod drift;
mod error;
mod events;
mod facts;
mod gate;
mod ids;
mod kill;
pub mod lexicon;
mod never_apps;
mod norm;
mod parse;
mod plan;
mod report;
mod tier;

pub use audit::{
    audit_label, AuditError, AuditRecord, AuditSink, MemoryAudit, NullAudit, MAX_AUDIT_LABEL_CHARS,
    REDACTED_LABEL,
};
pub use bind::{bind_step, BindContext, BindError, BoundStep};
pub use catalogue::{AppEntry, Catalogue, StaticCatalogue};
pub use chord::UnknownChord;
pub use classify::classify;
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{
    Mode, PolicyConfig, TaskOrigin, ACTION_TTL, INPUT_GUARD, MAX_MINTS_PER_MINUTE, MIN_MINT_GAP,
    OBSERVATION_MAX_AGE, PLAN_APPROVAL_TIMEOUT, PLAN_GRANT_TTL, STALL_TTL, STEP_APPROVAL_TIMEOUT,
};
pub use error::PlanError;
pub use events::{
    AgentEvent, AgentEventKind, ApprovalKind, DenyWhy, EventSink, MemoryEvents, NullEvents,
};
pub use facts::{BoundStepFacts, FactOp, PolicyContext};
pub use gate::{
    ApprovedAction, Decision, DenyReason, EndReason, Evaluation, ExecKind, ExecResult, GateError,
    PendingPlanApproval, PendingStepApproval, PlanGrant, PolicyGate, Refusal, Revoked, StepGrant,
    VoidReason,
};
pub use ids::{ActionDigest, ApprovalId, TaskId};
pub use kill::{KillReason, KillSwitch};
pub use never_apps::NeverApps;
pub use plan::{
    validate, validate_bytes, PlanContext, PlanDigest, PlanLimits, PlanStep, Provenance, Target,
    ValidatedPlan, MAX_APP_NAME_CHARS, MAX_DRAFT_BYTES, MAX_LABEL_CHARS, MAX_QUERY_CHARS,
    MAX_STEPS, MAX_TRANSCRIPT_BYTES, MAX_TYPE_TEXT_CHARS,
};
pub use report::{DryBound, DryRunReport, DryRunStep, ProposedPlan};
pub use tier::{Classification, ReasonCode, Tier};

#[cfg(test)]
#[path = "../tests/common/world.rs"]
mod world;

// Tests (unit and integration) refer to the crate by its own name.
#[cfg(test)]
extern crate self as policy;
