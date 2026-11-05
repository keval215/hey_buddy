//! Drift rules (ADR 0003 section 7): what changed between what the user was shown at plan
//! approval and the step as bound NOW. Drift never lowers anything: it turns a batch-covered step
//! into a single-step approval ("changed since you approved"), or aborts when the step became
//! Never (that case is decided by the caller from the classification).

use crate::bind::BoundStep;
use crate::plan::PlanStep;
use crate::report::DryBound;
use crate::tier::{Classification, Tier};

/// What the user saw for one step at plan approval.
#[derive(Clone)]
pub(crate) struct Shown {
    /// The tier cap: the dry-run classification, or the provisional one for an unbound step.
    pub(crate) cap: Classification,
    /// `None` = the step could not be bound in the dry run.
    pub(crate) binding: Option<DryBound>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drift {
    /// Same as shown.
    None,
    /// Something listed in section 7 changed.
    Changed,
    /// Never shown bound and the step is Consequential or higher: the exact target was not
    /// approved, so it is asked for now.
    Unbound,
}

/// Moved more than 24 px or resized more than 25 %.
pub(crate) fn rect_drifted(a: (i32, i32, u32, u32), b: (i32, i32, u32, u32)) -> bool {
    let moved = (i64::from(a.0) - i64::from(b.0)).abs() > 24
        || (i64::from(a.1) - i64::from(b.1)).abs() > 24;
    let resized = |old: u32, new: u32| {
        let (old, new) = (i64::from(old), i64::from(new));
        (new - old).abs() * 4 > old
    };
    moved || resized(a.2, b.2) || resized(a.3, b.3)
}

/// True when the classification rose: a higher tier, or a step that became a submit.
pub(crate) fn escalated(shown: Classification, now: Classification) -> bool {
    now.tier > shown.tier || (now.submit && !shown.submit)
}

pub(crate) fn drift(
    shown: &Shown,
    step: &BoundStep,
    now: Classification,
    previous: Option<&PlanStep>,
) -> Drift {
    let Some(b) = &shown.binding else {
        return if now.tier >= Tier::Consequential {
            Drift::Unbound
        } else {
            Drift::None
        };
    };
    if escalated(shown.cap, now) {
        return Drift::Changed;
    }
    // A different foreground app, except right after a planned open_app / focus_window.
    let after_launch = matches!(
        previous,
        Some(PlanStep::OpenApp { .. } | PlanStep::FocusWindow { .. })
    );
    let app_now = step.facts().app_id.as_ref();
    if !after_launch && app_now != Some(&b.app) {
        return Drift::Changed;
    }
    // A payment context that appeared.
    if step.payment_context() && !b.payment {
        return Drift::Changed;
    }
    match (step.descriptor(), b.has_element) {
        (Some(d), true) => {
            let now_rect = (d.rect.x, d.rect.y, d.rect.w, d.rect.h);
            if d.element.node != b.node
                || d.role != b.role
                || d.label_sha256 != b.label_sha
                || d.editable != b.editable
                || rect_drifted(b.rect, now_rect)
            {
                return Drift::Changed;
            }
        }
        (None, false) => {}
        // An element appeared or vanished.
        _ => return Drift::Changed,
    }
    Drift::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_rules() {
        let r = (100, 100, 200, 40);
        assert!(!rect_drifted(r, r));
        assert!(!rect_drifted(r, (124, 76, 200, 40)));
        assert!(rect_drifted(r, (125, 100, 200, 40)));
        assert!(rect_drifted(r, (100, 75, 200, 40)));
        assert!(!rect_drifted(r, (100, 100, 250, 50)));
        assert!(rect_drifted(r, (100, 100, 251, 40)));
        assert!(rect_drifted(r, (100, 100, 200, 51)));
        assert!(rect_drifted((0, 0, 0, 0), (0, 0, 1, 0)));
    }

    #[test]
    fn escalation_rules() {
        use crate::tier::ReasonCode;
        let c = |tier, submit| Classification {
            tier,
            submit,
            batchable: false,
            reason: ReasonCode::ClickBaseline,
        };
        assert!(escalated(
            c(Tier::Reversible, false),
            c(Tier::Consequential, false)
        ));
        assert!(escalated(
            c(Tier::Consequential, false),
            c(Tier::Consequential, true)
        ));
        assert!(!escalated(
            c(Tier::Consequential, true),
            c(Tier::Reversible, false)
        ));
    }
}
