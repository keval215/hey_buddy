//! hey_buddy policy, part 1 (ADR 0003 sections 4 and 5): the strict `plan/v1` model and its
//! re-validation, provenance, the sealed [`ValidatedPlan`] with its digest, and the tier table as
//! the pure function [`classify`]. See `docs/policy.md`.
//!
//! NOT here yet (S2-11b): the gate, grants, approvals, `ApprovedAction`, observe mode, kill.
//!
//! Pure: depends only on `action`, `firewall`, `frame`, serde/serde_json (to parse the untrusted
//! draft), sha2 and thiserror. No tokio, llm, capture, voice, driver or OS crates (CI checks).
//! TODO(S2-15): add the matching CI rule that the `agent` crate does not depend on `policy`,
//! `driver`, `capture`, `firewall` or `llm` (`cargo tree -p agent`).
//!
//! # A `ValidatedPlan` cannot be forged
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

mod catalogue;
mod chord;
mod classify;
mod error;
mod facts;
pub mod lexicon;
mod never_apps;
mod norm;
mod parse;
mod plan;
mod tier;

pub use catalogue::{AppEntry, Catalogue, StaticCatalogue};
pub use chord::UnknownChord;
pub use classify::classify;
pub use error::PlanError;
pub use facts::{BoundStepFacts, FactOp, PolicyContext};
pub use never_apps::NeverApps;
pub use plan::{
    validate, validate_bytes, PlanContext, PlanDigest, PlanLimits, PlanStep, Provenance, Target,
    ValidatedPlan, MAX_APP_NAME_CHARS, MAX_DRAFT_BYTES, MAX_LABEL_CHARS, MAX_QUERY_CHARS,
    MAX_STEPS, MAX_TRANSCRIPT_BYTES, MAX_TYPE_TEXT_CHARS,
};
pub use tier::{Classification, ReasonCode, Tier};
