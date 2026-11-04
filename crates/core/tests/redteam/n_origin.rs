//! Case n: scheduled / background origin tries a Reversible or Consequential step; runaway loops.

use std::time::Duration;

use crate::rig::*;
use buddy_core::agent::{AgentNotice, PauseReason, RequestOrigin, RunLimits, TaskOutcome};
use driver::fake::CallKind;
use policy::{PolicyConfig, TaskOrigin};

/// Attack: a SCHEDULED task (nobody watching) plans `type`, `open_app`, `scroll`, a plain click and
/// a submit. Expected: `Paused(needs_user)` at the first step above Read, one notice, an audit
/// `sup_paused`, NOTHING executed, no approval window raised. Defence: scheduled origin is
/// read-only at the gate (stricter than the ticket text); a pause ends the task.
#[tokio::test]
async fn a_scheduled_task_never_executes_anything_above_read() {
    let steps = [
        type_into("Name", TYPED_CANARY),
        OPEN_NOTEPAD.to_owned(),
        SCROLL1.to_owned(),
        click("Refresh"),
        click("Save"),
    ];
    for step in steps {
        let e = env();
        let r = e
            .run_as(RequestOrigin::Scheduled, &steps_json(&[&step]))
            .await;
        assert_eq!(
            r.outcome,
            TaskOutcome::Paused(PauseReason::NeedsUser),
            "{step}: {r:?}"
        );
        e.executed_nothing();
        assert_eq!(
            e.notes.notices(),
            vec![AgentNotice::NeedsUser { task: r.task }]
        );
        assert!(e.events().contains(&"sup_paused"));
        assert!(!e.audit_text().contains(TYPED_CANARY));
        e.gate_idle();
    }
}

/// Attack: a BACKGROUND ("run now, then walk away") task plans a submit or a plain consequential
/// click. Expected: paused, never clicked. Defence: Background = Read + Reversible only.
#[tokio::test]
async fn a_manual_task_never_clicks_a_consequential_or_submit_step() {
    for label in ["Save", "Refresh"] {
        let e = env();
        let r = e
            .run_as(RequestOrigin::Manual, &steps_json(&[&click(label)]))
            .await;
        assert_eq!(
            r.outcome,
            TaskOutcome::Paused(PauseReason::NeedsUser),
            "{label}: {r:?}"
        );
        assert_eq!(e.fake.count(CallKind::Invoke), 0, "{label}");
        e.gate_idle();
    }
    // A destructive click is refused outright at the dry run (Never), not even paused.
    let e = env();
    let r = e
        .run_as(RequestOrigin::Manual, &steps_json(&[&click("Delete")]))
        .await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(buddy_core::agent::RefuseReason::PlanRefusedStep)
    );
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    // Typing then submitting: the typing may run (Reversible), the submit never.
    let e = env();
    let r = e.run_as(RequestOrigin::Manual, &form_plan()).await;
    assert!(matches!(r.outcome, TaskOutcome::Paused(_)), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "the submit never ran");
}

/// Attack: the gate itself is configured as SCHEDULED but the request claims to be interactive
/// (a mislabelled trigger). Expected: the stricter origin wins: paused, nothing runs.
/// Defence: `propose_as` takes the stricter of the request's and the gate's own origin.
#[tokio::test]
async fn the_stricter_of_the_gates_and_the_requests_origin_wins() {
    let e = env_with(Opts {
        config: Some(PolicyConfig {
            origin: TaskOrigin::Scheduled,
            ..PolicyConfig::default()
        }),
        ..Opts::default()
    });
    let r = e
        .run_as(
            RequestOrigin::Interactive,
            &steps_json(&[&type_into("Name", TYPED_CANARY)]),
        )
        .await;
    assert!(matches!(r.outcome, TaskOutcome::Paused(_)), "{r:?}");
    e.executed_nothing();
}

/// Attack: a runaway loop in an unattended task (the same scroll over and over).
/// Expected: stopped after the unattended repeat limit (3): `Stopped(runaway)`, a notice, the
/// kill switch tripped (everything voided), exactly 3 scrolls ran. Defence: `RunLimits::unattended`.
#[tokio::test]
async fn an_unattended_runaway_loop_is_stopped_at_the_unattended_limit() {
    let e = env();
    let r = e
        .run_as(
            RequestOrigin::Manual,
            &steps_json(&[SCROLL1, SCROLL1, SCROLL1, SCROLL1, SCROLL1]),
        )
        .await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("runaway"), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Scroll), 3);
    assert_eq!(
        e.notes.notices(),
        vec![AgentNotice::Stopped {
            task: r.task,
            why: "runaway"
        }]
    );
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    assert!(e.rig.kill.epoch() > 0, "the limit tripped the switch");
}

/// Attack: an unattended task with a very long plan of alternating steps, past the step budget
/// (8) and the time limit. Expected: `Stopped(step_budget)` / `Stopped(time_limit)`.
/// Defence: step budget and wall-clock limit in the supervisor.
#[tokio::test]
async fn the_unattended_step_budget_and_time_limit_stop_a_long_plan() {
    let alt: Vec<&str> = (0..12)
        .map(|i| if i % 2 == 0 { SCROLL1 } else { SCROLL_UP1 })
        .collect();
    let e = env();
    let r = e.run_as(RequestOrigin::Manual, &steps_json(&alt)).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("step_budget"), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Scroll), 8);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));

    let e = env_with(Opts {
        unattended: Some(RunLimits {
            max_steps: 8,
            max_repeat: 3,
            time_limit: Duration::from_secs(1),
        }),
        ..Opts::default()
    });
    let r = e.run_as(RequestOrigin::Manual, &steps_json(&alt)).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("time_limit"), "{r:?}");
    assert!(r.executed < 8);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}
