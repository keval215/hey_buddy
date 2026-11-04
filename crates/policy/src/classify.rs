//! The ADR 0003 section 5 tier table as one pure function. The result is the MAXIMUM over all
//! matching rows; facts only raise; unknown role => Consequential; unknown secure-ness / app id /
//! integrity / session => Never. Row numbers in comments are the ADR's.
//!
//! Stricter-than-the-table readings (all listed in `docs/policy.md`):
//! - rows 10/11 also apply to the `submit` op; row 14's secure-field rule applies to every
//!   targeted op, and its focused-secure rule to every keyboard-like op (type, submit, key,
//!   chord, copy, unknown);
//! - row 24's Never-app list and elevation apply to Read ops too (restricted apps stay
//!   observable, as in ADR 0004 section 8);
//! - a `type` into a target not known to be editable is asked for every time;
//! - every non-Read step with a missing environment fact is Never, and a missing session fact is
//!   Never for every op except observe;
//! - open_app / focus_window also run every app check on the TARGET app (`target_app`,
//!   `target_denied`); a chord containing Enter is a submit.

use firewall::{NodeRole, SessionState};

use crate::chord::{ALT, CTRL, SHIFT};
use crate::facts::{BoundStepFacts, FactOp, PolicyContext};
use crate::lexicon;
use crate::never_apps::is_file_manager;
use crate::plan::Provenance;
use crate::tier::{Classification, ReasonCode, Tier};
use action::KeyCombo;

struct Acc {
    tier: Tier,
    reason: ReasonCode,
    submit: bool,
    batchable: bool,
}

impl Acc {
    fn set(&mut self, tier: Tier, reason: ReasonCode, batchable: bool) {
        self.tier = tier;
        self.reason = reason;
        self.batchable = batchable;
    }

    /// Facts only raise: a row replaces the reason only if it is strictly higher.
    fn raise(&mut self, tier: Tier, reason: ReasonCode) {
        if tier > self.tier {
            self.tier = tier;
            self.reason = reason;
        }
    }
}

/// Classify one bound step. Deterministic and pure.
pub fn classify(f: &BoundStepFacts, ctx: &PolicyContext) -> Classification {
    let mut a = Acc {
        tier: Tier::Read,
        reason: ReasonCode::FirewallRead,
        submit: false,
        batchable: false,
    };
    baseline(f, &mut a);
    fact_rows(f, ctx, &mut a);
    if a.tier == Tier::Never {
        a.submit = false;
    }
    Classification {
        tier: a.tier,
        submit: a.submit,
        // Only the explicit "yes" rows survive; Never and submit are never batchable.
        batchable: a.batchable && !a.submit && a.tier != Tier::Never,
        reason: a.reason,
    }
}

fn baseline(f: &BoundStepFacts, a: &mut Acc) {
    use ReasonCode as R;
    use Tier as T;
    match &f.op {
        FactOp::Observe => a.set(T::Read, R::FirewallRead, true), // 1
        FactOp::FindText => a.set(T::Read, R::FindText, true),    // 2
        FactOp::AnswerFromScreen => a.set(T::Read, R::AnswerFromScreen, true), // 3
        FactOp::Scroll => a.set(T::Reversible, R::Scroll, true),  // 4
        FactOp::FocusWindow => a.set(T::Reversible, R::FocusWindow, true), // 5
        FactOp::OpenApp => a.set(T::Reversible, R::OpenApp, true), // 6
        FactOp::Click => match (f.role, f.editable) {
            // 7: view/focus only. (No tree role exists in NodeRole; tree expand/collapse is
            // `Other` and so row 8.)
            (Some(NodeRole::Tab | NodeRole::TextField), _)
            | (Some(NodeRole::ComboBox), Some(true)) => {
                a.set(T::Reversible, R::ClickFocusOnly, true)
            }
            // 8: everything else, including unknown role.
            _ => a.set(T::Consequential, R::ClickBaseline, false),
        },
        FactOp::Submit => {
            // 9
            a.set(T::Consequential, R::Submit, false);
            a.submit = true;
        }
        FactOp::Type => match (f.provenance, f.editable) {
            // 12
            (Some(Provenance::FromRequest), Some(true)) => {
                a.set(T::Consequential, R::TypeFromRequest, true)
            }
            (Some(Provenance::FromRequest), _) => {
                a.set(T::Consequential, R::TypeTargetNotEditable, false)
            }
            // 13 (unknown provenance counts as generated)
            _ => a.set(T::Consequential, R::TypeModelGenerated, false),
        },
        FactOp::Key(k) => match k {
            KeyCombo::Enter => {
                // 17
                a.set(T::Consequential, R::KeyEnter, false);
                a.submit = true;
            }
            k if k.is_navigation_only() => a.set(T::Reversible, R::KeyNavigation, true), // 15
            KeyCombo::SelectAll => a.set(T::Reversible, R::KeyClipboard, true),          // 16
            // 18: Space, Backspace, Delete, Undo, Find ...
            _ => a.set(T::Consequential, R::KeyOther, false),
        },
        FactOp::Chord(c) => {
            if c.has_win() || c.is(CTRL | ALT, "delete") || c.is(CTRL | SHIFT, "escape") {
                a.set(T::Never, R::KeyNever, false); // 19
            } else if c.key_is("enter") {
                a.set(T::Consequential, R::KeyEnter, false); // 17
                a.submit = true;
            } else {
                a.set(T::Consequential, R::KeyOther, false); // 18
            }
        }
        FactOp::CopySelection => a.set(T::Reversible, R::KeyClipboard, true), // 16
        FactOp::Unknown => a.set(T::Consequential, R::UnknownAction, false),  // 26
    }
}

fn is_targeted(op: &FactOp) -> bool {
    matches!(op, FactOp::Click | FactOp::Type | FactOp::Submit)
}

fn is_keyboard_like(op: &FactOp) -> bool {
    matches!(
        op,
        FactOp::Type
            | FactOp::Submit
            | FactOp::Key(_)
            | FactOp::Chord(_)
            | FactOp::CopySelection
            | FactOp::Unknown
    )
}

fn fact_rows(f: &BoundStepFacts, ctx: &PolicyContext, a: &mut Acc) {
    use ReasonCode as R;
    use Tier as T;
    let read = f.op.is_read();

    // 20: deny-listed target / foreground app.
    if f.is_denied_app == Some(true) {
        a.raise(T::Never, R::DeniedApp);
    }
    // open_app / focus_window: the TARGET app is judged like the foreground app. Unknown or denied
    // target verdict => Never; the Never-app list and installer heuristics apply to its id.
    let targets_app = matches!(f.op, FactOp::OpenApp | FactOp::FocusWindow);
    if targets_app {
        if f.target_denied == Some(true) {
            a.raise(T::Never, R::DeniedApp);
        }
        if f.target_app
            .as_ref()
            .is_some_and(|app| ctx.never_apps.contains(app))
        {
            a.raise(T::Never, R::NeverApp);
        }
    }
    // 21: session not Active (observe then fails by itself, so it keeps its tier).
    if f.op != FactOp::Observe {
        match &f.session {
            Some(SessionState::Active) => {}
            Some(_) => a.raise(T::Never, R::SessionNotActive),
            None => {} // missing: Never/MissingFact at the end, so a specific reason wins
        }
    }
    // 22: our own process.
    if f.is_own_process == Some(true) {
        a.raise(T::Never, R::OwnProcess);
    }
    // 23: payment context, any non-Read.
    if !read && f.payment_context == Some(true) {
        a.raise(T::Never, R::PaymentContext);
    }
    // 24: Never-apps and elevated windows (all ops); restricted apps (non-Read).
    if f.app_id
        .as_ref()
        .is_some_and(|app| ctx.never_apps.contains(app))
    {
        a.raise(T::Never, R::NeverApp);
    }
    if f.is_higher_integrity == Some(true) {
        a.raise(T::Never, R::ElevatedWindow);
    }
    if !read && f.is_restricted_app == Some(true) {
        a.raise(T::Never, R::RestrictedApp);
    }
    // 19: Delete-type chords in a file manager.
    // Any chord whose key is Delete (Shift+Delete, Ctrl+Shift+Delete ...) and Ctrl+D (delete in
    // many file managers) count; so does the app list's file-manager check.
    let delete_key = match &f.op {
        FactOp::Key(KeyCombo::Delete) => true,
        FactOp::Chord(c) => c.key_is("delete") || c.is(CTRL, "d"),
        _ => false,
    };
    if delete_key && f.app_id.as_ref().is_some_and(is_file_manager) {
        a.raise(T::Never, R::KeyNever);
    }

    // 14: secrets and payment data.
    if is_targeted(&f.op) && f.secure == Some(true) {
        a.raise(T::Never, R::SecureField);
    }
    if is_keyboard_like(&f.op) && f.focused_secure == Some(true) {
        a.raise(T::Never, R::FocusInSecureField);
    }
    let label = f.label.as_deref().unwrap_or("");
    let secret_label = f.sensitive_label || lexicon::is_secret(label);
    if f.op == FactOp::Type {
        if f.card_like_text {
            a.raise(T::Never, R::CardLikeText);
        }
        if secret_label {
            a.raise(T::Never, R::SensitiveLabel);
        }
    }
    if f.op == FactOp::Click
        && matches!(f.role, Some(NodeRole::TextField | NodeRole::ComboBox))
        && secret_label
    {
        a.raise(T::Never, R::SensitiveLabel);
    }

    // 10, 11, 9: label lexicons on click / submit.
    if matches!(f.op, FactOp::Click | FactOp::Submit) {
        if lexicon::is_destructive(label) {
            a.raise(T::Never, R::DestructiveLabel);
        }
        if lexicon::is_purchase(label) {
            a.raise(T::Never, R::PurchaseLabel);
        }
        if lexicon::is_submit(label) && a.tier <= T::Consequential {
            a.tier = T::Consequential;
            a.reason = R::Submit;
            a.submit = true;
        }
    }

    // Missing session: Never for every op except observe (reads included).
    if f.op != FactOp::Observe && f.session.is_none() {
        a.raise(T::Never, R::MissingFact);
    }
    // Fail closed: a non-Read step with an unknown environment fact is Never.
    if !read {
        let mut missing = f.app_id.is_none()
            || f.is_denied_app.is_none()
            || f.is_own_process.is_none()
            || f.is_higher_integrity.is_none()
            || f.is_restricted_app.is_none()
            || f.payment_context.is_none()
            || f.session.is_none();
        if is_targeted(&f.op) {
            missing |= f.secure.is_none();
        }
        if is_keyboard_like(&f.op) {
            missing |= f.focused_secure.is_none();
        }
        if targets_app {
            missing |= f.target_app.is_none() || f.target_denied != Some(false);
        }
        if missing {
            a.raise(T::Never, R::MissingFact);
        }
    }
}
