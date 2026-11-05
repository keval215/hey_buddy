//! Binding a plan step to the current observation (ADR 0003 sections 1 and 6, ADR 0004 section 6).
//!
//! `bind_step` resolves the plan's target `{role?, label, nth?}` DETERMINISTICALLY to exactly one
//! element of the observation: the label is compared case-folded with invisible characters
//! stripped (`norm::provenance` on both sides), the optional role must be equal, and `nth` (1-based,
//! in emission order) picks among several matches. Zero matches is `NoSuchElement`, several
//! matches without `nth` is `Ambiguous`; nothing is ever guessed, and secure or unaddressable
//! nodes have no descriptor so they can never match.
//!
//! The result, a [`BoundStep`], is sealed (private fields) and carries the `action::Action` (or
//! none for the abstract read operations and for chords outside `action::KeyCombo`), the
//! firewall-cleared [`TargetDescriptor`], the [`BoundStepFacts`] built from firewall-cleared data
//! only, and the [`Classification`]. A `BoundStep` is data, not authority: only
//! `PolicyGate::authorize` can turn one into an `ApprovedAction`.

use std::fmt;
use std::time::Duration;

use action::{Action, ScrollAmount, ScrollTarget};
use firewall::{ElementRef, Observation, ObservationId, SessionState, TargetDescriptor};

use crate::classify::classify;
use crate::facts::{BoundStepFacts, PolicyContext};
use crate::norm;
use crate::plan::{PlanStep, Target};
use crate::tier::Classification;

/// Why a step could not be bound. Fixed tokens only (L-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindError {
    /// No addressable element matches role + label (+ nth).
    #[error("no_such_element")]
    NoSuchElement,
    /// Several elements match and the plan gave no `nth`.
    #[error("ambiguous")]
    Ambiguous,
    /// The observation is not the one `BindContext::observation` names.
    #[error("stale_observation")]
    StaleObservation,
    /// A key chord or copy needs the element that has keyboard focus (`BindContext::focus_target`).
    #[error("no_focus_target")]
    NoFocusTarget,
}

impl BindError {
    pub fn code(self) -> &'static str {
        match self {
            BindError::NoSuchElement => "no_such_element",
            BindError::Ambiguous => "ambiguous",
            BindError::StaleObservation => "stale_observation",
            BindError::NoFocusTarget => "no_focus_target",
        }
    }
}

/// What the caller (core, from `Firewall::preflight` and the driver) knows besides the
/// observation. Facts that cannot be derived from the `Observation` stay `None` (= unknown) and
/// make a non-Read step Never (`missing_fact`).
#[derive(Debug, Clone)]
pub struct BindContext {
    pub policy: PolicyContext,
    /// A FRESH `Firewall::preflight()` result.
    pub session: SessionState,
    /// The id of the observation the caller considers current; binding any other is refused.
    pub observation: ObservationId,
    /// Index of the plan step being bound.
    pub step_index: u16,
    /// `Clock::now()` of the gate's clock.
    pub now: Duration,
    /// The element of THIS observation that has keyboard focus (key chords, copy).
    pub focus_target: Option<ElementRef>,
    /// Keyboard focus is inside a secure field. `None` = unknown.
    pub focused_secure: Option<bool>,
    /// The foreground process runs at a higher integrity level than ours. `None` = unknown.
    pub higher_integrity: Option<bool>,
    /// `open_app` / `focus_window`: the target app is NOT denied, restricted, elevated or ours.
    pub target_denied: Option<bool>,
}

impl BindContext {
    pub fn new(
        policy: PolicyContext,
        session: SessionState,
        observation: ObservationId,
        now: Duration,
    ) -> Self {
        Self {
            policy,
            session,
            observation,
            step_index: 0,
            now,
            focus_target: None,
            focused_secure: None,
            higher_integrity: None,
            target_denied: None,
        }
    }

    pub fn for_step(&self, index: u16) -> Self {
        let mut c = self.clone();
        c.step_index = index;
        c
    }
}

/// One plan step bound to one observation. Sealed: private fields, no `Clone`, no `Default`, no
/// serde. Debug prints the index, op and tier only.
pub struct BoundStep {
    index: u16,
    plan_step: PlanStep,
    action: Option<Action>,
    descriptor: Option<TargetDescriptor>,
    facts: BoundStepFacts,
    classification: Classification,
    observation: ObservationId,
    bound_at: Duration,
    payment: bool,
}

impl BoundStep {
    pub fn index(&self) -> u16 {
        self.index
    }
    /// The abstract plan step this was bound from.
    pub fn plan_step(&self) -> &PlanStep {
        &self.plan_step
    }
    /// `None` for `observe` / `find_text` / `answer_from_screen` (data only, never reach the
    /// driver) and for chords outside `action::KeyCombo` (never executable).
    pub fn action(&self) -> Option<&Action> {
        self.action.as_ref()
    }
    pub fn descriptor(&self) -> Option<&TargetDescriptor> {
        self.descriptor.as_ref()
    }
    pub fn classification(&self) -> Classification {
        self.classification
    }
    pub fn observation(&self) -> ObservationId {
        self.observation
    }
    pub fn bound_at(&self) -> Duration {
        self.bound_at
    }
    pub fn payment_context(&self) -> bool {
        self.payment
    }
    pub(crate) fn facts(&self) -> &BoundStepFacts {
        &self.facts
    }
}

impl fmt::Debug for BoundStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BoundStep(#{} {}, {})",
            self.index,
            self.plan_step.op_name(),
            self.classification.tier.as_str()
        )
    }
}

fn resolve(target: &Target, obs: &Observation) -> Result<TargetDescriptor, BindError> {
    let want = norm::provenance(target.label());
    if want.is_empty() {
        return Err(BindError::NoSuchElement);
    }
    let found: Vec<TargetDescriptor> = obs
        .elements()
        .filter(|d| target.role().is_none_or(|r| d.role == r))
        .filter(|d| {
            d.label
                .as_deref()
                .is_some_and(|l| norm::provenance(l) == want)
        })
        .collect();
    match (target.nth(), found.len()) {
        (_, 0) => Err(BindError::NoSuchElement),
        (None, 1) => found.into_iter().next().ok_or(BindError::NoSuchElement),
        (None, _) => Err(BindError::Ambiguous),
        (Some(n), len) if usize::from(n) <= len && n >= 1 => found
            .into_iter()
            .nth(usize::from(n) - 1)
            .ok_or(BindError::NoSuchElement),
        (Some(_), _) => Err(BindError::NoSuchElement),
    }
}

/// Binds `step` to `obs`. See the module docs. Executes nothing and mints nothing.
pub fn bind_step(
    step: &PlanStep,
    obs: &Observation,
    ctx: &BindContext,
) -> Result<BoundStep, BindError> {
    if obs.id() != ctx.observation {
        return Err(BindError::StaleObservation);
    }
    let mut descriptor: Option<TargetDescriptor> = None;
    let action: Option<Action> = match step {
        PlanStep::Observe | PlanStep::FindText { .. } | PlanStep::AnswerFromScreen { .. } => None,
        PlanStep::KeyChord(_) => None,
        PlanStep::Scroll { direction, amount } => Action::scroll(
            ScrollTarget::Window(obs.id()),
            *direction,
            ScrollAmount::SmallStep,
            *amount,
        )
        .ok(),
        PlanStep::FocusWindow { app, .. } => Some(Action::focus_app(app.app.clone())),
        PlanStep::OpenApp { app, .. } => Some(Action::open_app(app.launch.clone())),
        PlanStep::Click { target } | PlanStep::Submit { target } => {
            let d = resolve(target, obs)?;
            let a = Action::click(d.element);
            descriptor = Some(d);
            Some(a)
        }
        PlanStep::Type { target, text, .. } => {
            let d = resolve(target, obs)?;
            let a = Action::type_text(d.element, text.clone());
            descriptor = Some(d);
            Some(a)
        }
        PlanStep::KeyCombo(_) | PlanStep::CopySelection => {
            let r = ctx.focus_target.ok_or(BindError::NoFocusTarget)?;
            let d = obs.describe(r).ok_or(BindError::NoSuchElement)?;
            let a = match step {
                PlanStep::KeyCombo(k) => Action::key_combo(d.element, *k),
                _ => Action::copy_selection(d.element),
            };
            descriptor = Some(d);
            Some(a)
        }
    };

    let payment = obs.flags().payment_suspected;
    let mut facts = BoundStepFacts::for_step(step)
        .app(obs.app_id().clone())
        .own_process(false) // an observation of our own window is never produced
        .denied_app(false) // nor one of a deny-listed app
        .restricted_app(obs.is_restricted_app())
        .payment_context(payment)
        .session(ctx.session.clone());
    if let Some(b) = ctx.higher_integrity {
        facts = facts.higher_integrity(b);
    }
    if let Some(b) = ctx.focused_secure {
        facts = facts.focused_secure(b);
    }
    if let Some(b) = ctx.target_denied {
        facts = facts.target_denied(b);
    }
    if let Some(d) = &descriptor {
        // `describe` never yields a secure node, so a bound target is not secure.
        facts = facts
            .role(d.role)
            .editable(d.editable)
            .secure(false)
            .sensitive_label(d.sensitive_label);
        if let Some(l) = d.label.as_deref() {
            facts = facts.label(l);
        }
        // Joined-label lexicons computed in the firewall (stricter only; policy's own check on
        // the displayed label still runs).
        if let Some(fl) = obs.describe_flags(d.element) {
            facts = facts.label_flags(fl);
        }
    } else if matches!(
        step,
        PlanStep::Click { .. } | PlanStep::Type { .. } | PlanStep::Submit { .. }
    ) {
        // Unreachable (those steps always have a descriptor); fail closed regardless.
        return Err(BindError::NoSuchElement);
    }
    let classification = classify(&facts, &ctx.policy);
    Ok(BoundStep {
        index: ctx.step_index,
        plan_step: step.clone(),
        action,
        descriptor,
        facts,
        classification,
        observation: obs.id(),
        bound_at: ctx.now,
        payment,
    })
}
