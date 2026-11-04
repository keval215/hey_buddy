//! Case i: approval replay and forgery. The "human" misbehaves inside `ApprovalUi::show`.

use std::sync::{Arc, Mutex};

use crate::rig::*;
use buddy_core::agent::{Stage, TaskOutcome};
use buddy_core::approval::{ApprovalDecisionDto, RespondError};
use driver::fake::CallKind;

/// Attack: the same approval id and digest are answered twice. Expected: the second answer is
/// `UnknownId`; the task ran once on the first. Defence: a pending approval is removed when
/// resolved (single use).
#[tokio::test]
async fn the_same_approval_answered_twice_is_refused_the_second_time() {
    let e = env();
    e.human.script(&[custom(|cx| {
        cx.display();
        cx.advance_ms(600);
        cx.approve().unwrap();
        assert_eq!(cx.approve(), Err(RespondError::UnknownId));
        assert_eq!(
            cx.respond(ApprovalDecisionDto::Deny, &cx.req.digest_hex.clone()),
            Err(RespondError::UnknownId),
            "a deny cannot flip an approved request either"
        );
    })]);
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(e.human.seen().len(), 1);
    e.gate_idle();
}

/// Attack: an approval (id and digest) from a finished task is replayed into the next task's
/// request. Expected: `UnknownId` for the old id; the old digest does not fit the new request
/// (`DigestMismatch`); the new request still needs its own answer (here: denied).
/// Defence: random single-use ids; the digest binds the plan.
#[tokio::test]
async fn an_approval_from_an_earlier_task_cannot_be_replayed_into_the_next() {
    let e = env();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    let old = e.human.seen()[0].clone();
    e.gate_idle();
    let old2 = old.clone();
    e.human.script(&[custom(move |cx| {
        assert_eq!(
            cx.broker
                .respond(&old2.id, ApprovalDecisionDto::Approve, &old2.digest_hex),
            Err(RespondError::UnknownId)
        );
        cx.display();
        cx.advance_ms(600);
        // The old digest on the NEW id.
        assert_eq!(
            cx.respond(ApprovalDecisionDto::Approve, &old2.digest_hex),
            Err(RespondError::DigestMismatch)
        );
        cx.deny().unwrap();
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    assert_ne!(old.digest_hex, e.human.seen()[1].digest_hex);
}

/// Attack: the window answers "approve" before it ever displayed the request. Expected:
/// `NotDisplayed`; nothing runs (the request is then denied). Defence: the broker needs
/// `mark_displayed` before an approve counts.
#[tokio::test]
async fn an_approve_before_the_request_was_displayed_is_refused() {
    let e = env();
    e.human.script(&[custom(|cx| {
        assert_eq!(cx.approve(), Err(RespondError::NotDisplayed));
        cx.deny().unwrap();
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

/// Attack: a click-through: the approve arrives 0 ms, 100 ms and 499 ms after display (a script,
/// or a keystroke the user was already making). Expected: `TooEarly` each time; the approval
/// counts only from 500 ms. Defence: the 500 ms input guard (`INPUT_GUARD`).
#[tokio::test]
async fn an_approve_inside_the_500_ms_guard_is_refused_and_counts_from_500_ms() {
    let e = env();
    e.human.script(&[custom(|cx| {
        cx.display();
        assert_eq!(cx.approve(), Err(RespondError::TooEarly), "0 ms");
        cx.advance_ms(100);
        assert_eq!(cx.approve(), Err(RespondError::TooEarly), "100 ms");
        cx.advance_ms(399);
        assert_eq!(cx.approve(), Err(RespondError::TooEarly), "499 ms");
        // A re-display (reloaded window) restarts the guard, never shortens it.
        cx.display();
        cx.advance_ms(499);
        assert_eq!(
            cx.approve(),
            Err(RespondError::TooEarly),
            "499 ms after a re-display"
        );
        cx.advance_ms(1);
        cx.approve().unwrap();
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(e.fake.count(CallKind::Launch), 1);
}

/// Attack: the answer echoes the wrong digest: all zeros, the right digest in upper case, a
/// prefix, and the digest with a character appended. Expected: `DigestMismatch` every time; only
/// the exact digest approves. Defence: constant-time exact compare of the full hex digest.
#[tokio::test]
async fn a_digest_of_another_plan_or_a_near_miss_is_refused() {
    let e = env();
    e.human.script(&[custom(|cx| {
        cx.display();
        cx.advance_ms(600);
        let good = cx.req.digest_hex.clone();
        for bad in [
            "0".repeat(64),
            good.to_uppercase(),
            good[..16].to_owned(),
            format!("{good}0"),
            String::new(),
        ] {
            assert_eq!(
                cx.respond(ApprovalDecisionDto::Approve, &bad),
                Err(RespondError::DigestMismatch),
                "{bad:?}"
            );
        }
        cx.deny().unwrap();
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
}

/// Attack: the plan approval's digest is used to answer the later STEP approval (the submit).
/// Expected: `DigestMismatch`; the click never happens. Defence: each approval has its own digest.
#[tokio::test]
async fn a_plan_approvals_digest_cannot_answer_the_submit_approval() {
    let e = env();
    let plan_digest = Arc::new(Mutex::new(String::new()));
    let (p1, p2) = (plan_digest.clone(), plan_digest.clone());
    e.human.script(&[
        custom(move |cx| {
            *p1.lock().unwrap() = cx.req.digest_hex.clone();
            cx.display();
            cx.advance_ms(600);
            cx.approve().unwrap();
        }),
        custom(move |cx| {
            cx.display();
            cx.advance_ms(600);
            let d = p2.lock().unwrap().clone();
            assert_eq!(
                cx.respond(ApprovalDecisionDto::Approve, &d),
                Err(RespondError::DigestMismatch)
            );
            cx.deny().unwrap();
        }),
    ]);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::StepApproval), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "no click");
    e.gate_idle();
}

/// Attack: the approval arrives after it expired. Expected: `Expired`, the task ends
/// `Expired(live_offer)`, nothing runs. Defence: expiry is checked when the answer arrives.
#[tokio::test]
async fn an_approval_after_expiry_is_refused() {
    let e = env();
    e.human.script(&[custom(|cx| {
        cx.display();
        cx.advance_ms(1_000_000);
        assert_eq!(cx.approve(), Err(RespondError::Expired));
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Expired(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

/// Attack: the kill switch fires and THEN the approval arrives. Expected: the answer is refused
/// (`Killed` / `UnknownId`), the task ends `Killed`, nothing runs. Defence: the broker resolves
/// every pending request `Killed` on a trip; the gate voids grants.
#[tokio::test]
async fn an_approval_after_the_kill_switch_is_refused() {
    let e = env();
    e.human.script(&[custom(|cx| {
        cx.display();
        cx.advance_ms(600);
        let ctl = cx.ctl.clone();
        std::thread::spawn(move || {
            ctl.trip(buddy_core::kill::KillReason::Hotkey);
        })
        .join()
        .unwrap();
        let r = cx.approve();
        assert!(
            matches!(r, Err(RespondError::Killed | RespondError::UnknownId)),
            "{r:?}"
        );
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    assert!(e.ctl.wait_idle(std::time::Duration::from_secs(10)));
}

/// Attack: a window that cannot be shown (the approval window failed to open). Expected: the
/// request is a Deny: `Denied`/`Blocked`, never an implicit yes. Defence: broker fails closed.
#[tokio::test]
async fn an_approval_that_cannot_be_shown_is_a_deny() {
    let e = env();
    e.human.script(&[Act::Unshowable]);
    let r = e.run(&notepad_plan()).await;
    assert!(failed_closed(&r), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

/// Attack: an approval answered from a window that is not the approval window (the settings
/// window, an overlay). The window-label check is command-level in the Tauri app and cannot run
/// here, so this is a source tripwire: every approval command checks the label BEFORE it touches
/// the broker (the desktop crate has its own unit tests for `label_is_*` and the capabilities).
/// Expected: the check precedes the broker call in each approval command. Defence: per-window
/// capability plus `only_approval_window`.
#[test]
fn approval_commands_check_the_window_label_before_they_touch_the_broker() {
    let src = include_str!("../../../../apps/desktop/src-tauri/src/commands.rs");
    for (name, call) in [
        ("approval_ready", ".pending_requests("),
        ("approval_displayed", ".mark_displayed("),
        ("approval_respond", ".respond("),
    ] {
        let at = src
            .find(&format!("pub fn {name}("))
            .unwrap_or_else(|| panic!("{name} not found"));
        let body = &src[at..at + 700];
        let check = body.find("only_approval_window(&window)?");
        let broker = body.find(call);
        assert!(check.is_some(), "{name} must check the window label");
        assert!(
            broker.is_some(),
            "{name}: control: the broker call is found"
        );
        assert!(
            check < broker,
            "{name}: the label check must come before the broker call"
        );
    }
}
