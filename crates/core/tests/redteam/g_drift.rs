//! Case g: stale refs, observation reuse, target swapped between plan and execute, foreground changed.

use crate::rig::*;
use buddy_core::agent::TaskOutcome;
use driver::fake::CallKind;
use firewall::fake::WindowSpec;
use firewall::SessionProbeState;

/// Attack: between the plan being shown and the typing, the field named "Name" is renamed (the
/// page swapped the control under the agent's feet). Expected: the fresh observation has no such
/// element: `Blocked(no_such_element)`, nothing typed, no intent. Defence: a fresh observation
/// and a fresh bind for EVERY step; descriptors are never reused across observations.
#[tokio::test]
async fn a_target_swapped_between_plan_and_execute_is_not_typed_into() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.human.script(&[custom(move |cx| {
        desk.rename(HWND, &[1], "Other");
        cx.display();
        cx.advance_ms(600);
        cx.approve().unwrap();
    })]);
    let r = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("no_such_element"), "{r:?}");
    assert_eq!(e.fake.chars_sent(), 0);
    assert!(e.rig.audit.intents().is_empty());
    e.gate_idle();
}

/// Attack: during the approval another app (with the same field names) takes the foreground.
/// Expected: nothing runs in it unless the user answers a SECOND question for the step. Defence:
/// the live observation is of the CURRENT foreground app; the gate's drift rule (a different
/// foreground app) turns a plan-approved step into a single-step approval.
#[tokio::test]
async fn a_foreground_change_before_the_first_step_types_nothing_into_the_new_app() {
    // A scroll (Reversible: it runs on the plan approval alone, so ONLY the drift rule can ask
    // again) and a type step (Consequential in the new app).
    for plan in [
        steps_json(&[SCROLL1]),
        steps_json(&[&type_into("Name", TYPED_CANARY)]),
    ] {
        let e = env();
        let desk = e.rig.desk.clone();
        // The plan approval is given; the live offer is followed by a SECOND question for the
        // step, because the step changed since it was shown (drift). The user says no to it.
        e.human.script(&[
            custom(move |cx| {
                desk.add_window(window_with(0x300, 77, "other.exe"));
                desk.set_foreground(0x300);
                cx.display();
                cx.advance_ms(600);
                cx.approve().unwrap();
            }),
            Act::Deny,
        ]);
        let r = e.run(&plan).await;
        assert_eq!(
            r.outcome,
            TaskOutcome::Denied(buddy_core::agent::Stage::StepApproval),
            "{plan}: {r:?}"
        );
        assert_eq!(e.fake.chars_sent(), 0, "typed into another app: {r:?}");
        assert_eq!(
            e.fake.count(CallKind::Scroll),
            0,
            "scrolled another app: {r:?}"
        );
        assert_eq!(e.human.seen().len(), 2, "the drifted step was asked again");
        e.gate_idle();
    }
}

/// Attack (finding F-2, closed in S2-22b): the second question after a foreground-app change.
/// Expected: it is flagged `changed` so the approval window warns "changed since approved".
/// Defence: the gate's own drift verdict (`PendingStepApproval::changed`) feeds the request.
#[tokio::test]
async fn the_second_question_after_an_app_change_says_it_changed() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.human.script(&[
        custom(move |cx| {
            desk.add_window(window_with(0x300, 77, "other.exe"));
            desk.set_foreground(0x300);
            cx.display();
            cx.advance_ms(600);
            cx.approve().unwrap();
        }),
        Act::Deny,
    ]);
    let _ = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert!(e.human.seen()[1].changed);
}

/// Same attack, but a user who says yes to the drifted step question is the user's call: the
/// text goes where the user just approved it to go. (Documented so nobody mistakes the previous
/// test for a guarantee against an approving human.)
#[tokio::test]
async fn a_drifted_step_runs_only_if_the_user_approves_the_changed_question() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.human.script(&[
        custom(move |cx| {
            desk.add_window(window_with(0x300, 77, "other.exe"));
            desk.set_foreground(0x300);
            cx.display();
            cx.advance_ms(600);
            cx.approve().unwrap();
        }),
        Act::Approve,
    ]);
    let r = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(e.human.seen().len(), 2);
    e.gate_idle();
}

/// Attack: another app takes the foreground right after the first action. Expected: the next
/// step does not run. Defence: fresh observation + drift per step.
#[tokio::test]
async fn a_foreground_change_after_the_first_action_stops_the_next_step() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            desk.add_window(WindowSpec::new(0x300, "x.exe", 7, WIN));
            desk.set_foreground(0x300);
        }
    });
    let r = e.run(&notepad_plan()).await;
    assert!(
        matches!(r.outcome, TaskOutcome::Blocked(_) | TaskOutcome::Failed(_)),
        "{r:?}"
    );
    assert_eq!(e.fake.chars_sent(), 0);
    e.gate_idle();
}

/// Attack: the session locks (or the secure desktop appears) between approval and the step.
/// Expected: the observation is paused, the step is `Blocked`, nothing is typed.
/// Defence: lock-screen pause in the firewall.
#[tokio::test]
async fn a_session_that_locks_after_approval_stops_the_step() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.human.script(&[custom(move |cx| {
        desk.set_session(SessionProbeState::LockScreen);
        cx.display();
        cx.advance_ms(600);
        cx.approve().unwrap();
    })]);
    let r = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert!(matches!(r.outcome, TaskOutcome::Blocked(_)), "{r:?}");
    assert_eq!(e.fake.chars_sent(), 0);
    e.gate_idle();
}

/// Attack: the element the plan clicked is replaced by an ambiguous twin (two "Twin" buttons).
/// Expected: the click is not made on a guess. Defence: ambiguous labels refuse to bind.
#[tokio::test]
async fn an_ambiguous_target_is_never_guessed() {
    let e = env();
    let r = e.run(&steps_json(&[&click("Twin")])).await;
    assert!(failed_closed(&r), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
}
