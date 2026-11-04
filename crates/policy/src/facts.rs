//! Pure data the classifier works from (ADR 0003 section 5: "inputs only from firewall-cleared
//! facts"). Everything environmental starts UNKNOWN (`None`); unknown facts escalate.

use action::{AppId, KeyCombo};
use firewall::{NodeRole, SessionState};

use crate::chord::UnknownChord;
use crate::lexicon;
use crate::never_apps::NeverApps;
use crate::norm;
use crate::plan::{PlanStep, Provenance};

/// The abstract operation, without any target or text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactOp {
    Observe,
    FindText,
    AnswerFromScreen,
    Scroll,
    FocusWindow,
    OpenApp,
    Click,
    Type,
    Submit,
    Key(KeyCombo),
    Chord(UnknownChord),
    CopySelection,
    /// An action kind this policy does not know (row 26).
    Unknown,
}

impl FactOp {
    pub fn of(step: &PlanStep) -> Self {
        match step {
            PlanStep::Observe => FactOp::Observe,
            PlanStep::FindText { .. } => FactOp::FindText,
            PlanStep::AnswerFromScreen { .. } => FactOp::AnswerFromScreen,
            PlanStep::Scroll { .. } => FactOp::Scroll,
            PlanStep::FocusWindow { .. } => FactOp::FocusWindow,
            PlanStep::OpenApp { .. } => FactOp::OpenApp,
            PlanStep::Click { .. } => FactOp::Click,
            PlanStep::Type { .. } => FactOp::Type,
            PlanStep::Submit { .. } => FactOp::Submit,
            PlanStep::KeyCombo(k) => FactOp::Key(*k),
            PlanStep::KeyChord(c) => FactOp::Chord(*c),
            PlanStep::CopySelection => FactOp::CopySelection,
        }
    }

    /// Rows 1-3: reads of firewall-cleared text.
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            FactOp::Observe | FactOp::FindText | FactOp::AnswerFromScreen
        )
    }
}

/// Firewall-cleared facts about one step at one moment. Build with [`BoundStepFacts::new`] or
/// [`BoundStepFacts::for_step`] and the setters; fields are public data, not capabilities.
///
/// "Environment" facts (`app_id`, `is_*`, `payment_context`, `session`, and for targeted ops
/// `secure` / `focused_secure`) MUST be supplied for every non-Read step: a missing one makes the
/// step Never (`ReasonCode::MissingFact`). For ops without a window of their own (`open_app`)
/// pass the named app and the foreground window's flags. `open_app` / `focus_window` also need
/// `target_app` and `target_denied == Some(false)` (the target may differ from the foreground).
#[derive(Clone, PartialEq, Eq)]
pub struct BoundStepFacts {
    pub op: FactOp,
    /// OBSERVED role of the target. `None` = unknown (Consequential for clicks).
    pub role: Option<NodeRole>,
    /// Sanitised label of the target (never raw OS text).
    pub label: Option<String>,
    pub editable: Option<bool>,
    pub secure: Option<bool>,
    pub focused_secure: Option<bool>,
    pub app_id: Option<AppId>,
    pub is_own_process: Option<bool>,
    pub is_higher_integrity: Option<bool>,
    pub is_restricted_app: Option<bool>,
    pub is_denied_app: Option<bool>,
    /// `open_app` / `focus_window` only: the app the step will launch or raise (seeded from the
    /// catalogue entry by [`BoundStepFacts::for_step`]). Checked like `app_id`.
    pub target_app: Option<AppId>,
    /// `open_app` / `focus_window` only: the CALLER's verdict that the target app is NOT denied,
    /// restricted, elevated or ours. `None` (unknown) and `Some(true)` are both Never.
    pub target_denied: Option<bool>,
    /// A payment label is visible in the window (the observation's `payment_suspected`).
    pub payment_context: Option<bool>,
    /// The firewall's `sensitive_label` flag for the target.
    pub sensitive_label: bool,
    /// `type` only; `None` is treated as model-generated.
    pub provenance: Option<Provenance>,
    /// `type` only: text is 13-19 digits passing Luhn.
    pub card_like_text: bool,
    pub session: Option<SessionState>,
}

impl std::fmt::Debug for BoundStepFacts {
    /// No label, no app id: op, role and the unknown-ness of the environment only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BoundStepFacts({:?}, role {:?})", self.op, self.role)
    }
}

impl BoundStepFacts {
    /// Everything unknown.
    pub fn new(op: FactOp) -> Self {
        Self {
            op,
            role: None,
            label: None,
            editable: None,
            secure: None,
            focused_secure: None,
            app_id: None,
            is_own_process: None,
            is_higher_integrity: None,
            is_restricted_app: None,
            is_denied_app: None,
            target_app: None,
            target_denied: None,
            payment_context: None,
            sensitive_label: false,
            provenance: None,
            card_like_text: false,
            session: None,
        }
    }

    /// Op, provenance and card-likeness from the validated step; the plan's own label is used as
    /// a placeholder (it can only escalate) until the observed label replaces it.
    pub fn for_step(step: &PlanStep) -> Self {
        let mut f = Self::new(FactOp::of(step));
        f.provenance = step.provenance();
        f.card_like_text = step
            .text()
            .is_some_and(|t| lexicon::looks_like_card_number(t.as_str()));
        f.label = step.target().map(|t| t.label().to_owned());
        f.target_app = match step {
            PlanStep::OpenApp { app, .. } | PlanStep::FocusWindow { app, .. } => {
                Some(app.app.clone())
            }
            _ => None,
        };
        f
    }

    pub fn role(mut self, r: NodeRole) -> Self {
        self.role = Some(r);
        self
    }
    /// Display-sanitises (`<`,`>`,`"` mapped, whitespace collapsed) with invisible characters
    /// REMOVED rather than spaced, so `Del<ZWSP>ete` is still one word to the lexicons.
    pub fn label(mut self, l: &str) -> Self {
        self.label = Some(norm::sanitise_joined(l));
        self
    }
    pub fn editable(mut self, b: bool) -> Self {
        self.editable = Some(b);
        self
    }
    pub fn secure(mut self, b: bool) -> Self {
        self.secure = Some(b);
        self
    }
    pub fn focused_secure(mut self, b: bool) -> Self {
        self.focused_secure = Some(b);
        self
    }
    pub fn app(mut self, a: AppId) -> Self {
        self.app_id = Some(a);
        self
    }
    pub fn own_process(mut self, b: bool) -> Self {
        self.is_own_process = Some(b);
        self
    }
    pub fn higher_integrity(mut self, b: bool) -> Self {
        self.is_higher_integrity = Some(b);
        self
    }
    pub fn restricted_app(mut self, b: bool) -> Self {
        self.is_restricted_app = Some(b);
        self
    }
    pub fn denied_app(mut self, b: bool) -> Self {
        self.is_denied_app = Some(b);
        self
    }
    pub fn target_app(mut self, a: AppId) -> Self {
        self.target_app = Some(a);
        self
    }
    pub fn target_denied(mut self, b: bool) -> Self {
        self.target_denied = Some(b);
        self
    }
    pub fn payment_context(mut self, b: bool) -> Self {
        self.payment_context = Some(b);
        self
    }
    pub fn sensitive_label(mut self, b: bool) -> Self {
        self.sensitive_label = b;
        self
    }
    pub fn session(mut self, s: SessionState) -> Self {
        self.session = Some(s);
        self
    }

    /// FOR FAKES AND TESTS ONLY (compiled out of production builds: `cfg(test)` or the
    /// off-by-default `test-support` feature): asserts every FOREGROUND environment fact is benign
    /// (known, not secure, not denied/own/elevated/restricted/payment, session Active). Does not
    /// touch the open_app / focus_window target facts. Production callers derive each from the
    /// firewall observation instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assume_benign_environment(self, app: AppId) -> Self {
        self.app(app)
            .own_process(false)
            .higher_integrity(false)
            .restricted_app(false)
            .denied_app(false)
            .payment_context(false)
            .secure(false)
            .focused_secure(false)
            .session(SessionState::Active)
    }
}

/// What the classifier needs besides the step: the Never-app list (ADR 0003 section 5 `NeverApps`).
#[derive(Debug, Clone, Default)]
pub struct PolicyContext {
    pub never_apps: NeverApps,
}

impl PolicyContext {
    pub fn new(never_apps: NeverApps) -> Self {
        Self { never_apps }
    }
}
