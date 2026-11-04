//! Gate tests that need access to internals to isolate ONE `authorize` condition (for example a
//! kill epoch that moved without the void hook running). Everything else is in `tests/`.

use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::world::*;

fn plan5() -> String {
    steps_json(&[
        OBSERVE,
        &click("Notes"),
        &type_into("Name", "John Smith"),
        &click("Refresh"),
        &click("Save"),
    ])
}

fn live5() -> (Rig, ProposedPlan, PlanGrant) {
    let rig = Rig::new();
    let (plan, grant) = rig.go_live(&plan5());
    (rig, plan, grant)
}

fn fresh_bind(rig: &Rig, plan: &ProposedPlan, i: usize) -> BoundStep {
    rig.advance(300);
    rig.bind(plan, i)
}

// ---- the happy path and each condition on its own --------------------------------------------

#[test]
fn happy_path_runs_the_whole_plan_and_finishes() {
    let (rig, plan, mut grant) = live5();
    for i in 0..5 {
        rig.run_step(&plan, &mut grant, i);
    }
    assert_eq!(rig.gate.mode(), Mode::Observe);
    let kinds = rig.events.kinds();
    assert!(kinds.contains(&AgentEventKind::TaskFinished {
        outcome: "completed"
    }));
    assert_eq!(rig.audit.intents().len(), 4);
}

#[test]
fn cond_observe_mode_never_mints() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.gate.lock().task.as_mut().unwrap().mode = Mode::Observe;
    let s = fresh_bind(&rig, &plan, 1);
    assert!(matches!(
        rig.gate.evaluate(&s, Some(&grant)),
        Evaluation::WouldDo(Tier::Reversible)
    ));
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::ObserveMode)
    );
    assert!(rig.audit.intents().is_empty());
}

#[test]
fn cond_grant_digest_must_be_the_frozen_plan() {
    let (rig, plan_a, mut grant_a) = live5();
    rig.run_step(&plan_a, &mut grant_a, 0);
    // Plan B becomes the live task; plan A's grant cannot run it.
    let (_plan_b, _grant_b) = rig.go_live(&plan5());
    let s = fresh_bind(&rig, &plan_a, 1);
    // Superseding bumped the void generation, so the old grant is dead on that count first.
    assert_eq!(
        rig.gate.authorize(s, &mut grant_a, None).err(),
        Some(Refusal::Voided)
    );
    // With the generation resynced by hand the digest check refuses on its own.
    grant_a.void_gen = rig.gate.void.load(Ordering::SeqCst);
    let s = fresh_bind(&rig, &plan_a, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant_a, None).err(),
        Some(Refusal::WrongPlan)
    );
}

#[test]
fn cond_step_index_must_be_next_and_is_never_reused() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    // Skipping ahead.
    let s = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::WrongStepIndex)
    );
    // Replay: step 1 minted once, the same grant step again is refused.
    rig.run_step(&plan, &mut grant, 1);
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::WrongStepIndex)
    );
    assert_eq!(grant.next_step(), 2);
    assert_eq!(rig.audit.intents().len(), 1);
}

#[test]
fn cond_op_must_equal_the_frozen_step() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    // Another plan's click on "Refresh" offered as step 1 (frozen: click "Notes").
    let alt = Rig::new().propose(&steps_json(&[&click("Refresh")]));
    rig.advance(300);
    let obs = rig.observe();
    rig.gate
        .note_observation(obs.id(), SessionState::Active)
        .unwrap();
    let wrong = bind_step(&alt.steps()[0], &obs, &rig.ctx(&obs, 1)).unwrap();
    assert_eq!(
        rig.gate.authorize(wrong, &mut grant, None).err(),
        Some(Refusal::OutOfPlan)
    );
}

#[test]
fn cond_observation_must_be_the_latest_noted_and_young() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    // (a) a newer observation was noted after binding.
    rig.advance(300);
    let obs_a = rig.observe();
    let s = rig.bind_in(&plan, 1, &obs_a);
    let obs_b = rig.observe();
    rig.gate
        .note_observation(obs_b.id(), SessionState::Active)
        .unwrap();
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Stale)
    );
    // (b) bound with an old timestamp (a caller-supplied clock reading 61 s ago).
    let obs = rig.observe();
    rig.gate
        .note_observation(obs.id(), SessionState::Active)
        .unwrap();
    let mut ctx = rig.ctx(&obs, 1);
    ctx.now = rig.gate.clock().now() - Duration::from_secs(61);
    let s = bind_step(&plan.steps()[1], &obs, &ctx).unwrap();
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Stale)
    );
    // (c) a spent observation cannot be noted again.
    let s = fresh_bind(&rig, &plan, 1);
    let spent_id = s.observation();
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    assert_eq!(
        rig.gate
            .note_observation(spent_id, SessionState::Active)
            .err(),
        Some(GateError::Refused(Refusal::Stale))
    );
}

#[test]
fn cond_fresh_session_must_be_active() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.advance(300);
    let obs = rig.observe();
    // The step was bound while Active, but the latest preflight says Paused.
    let s = {
        rig.gate
            .note_observation(obs.id(), SessionState::Active)
            .unwrap();
        bind_step(&plan.steps()[1], &obs, &rig.ctx(&obs, 1)).unwrap()
    };
    rig.gate
        .note_observation(
            obs.id(),
            SessionState::Paused(firewall::PauseReason::LockScreen),
        )
        .unwrap();
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::SessionNotActive)
    );
}

#[test]
fn cond_never_at_execution_aborts_the_task() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.run_step(&plan, &mut grant, 1);
    rig.run_step(&plan, &mut grant, 2);
    // A payment form appears on the page.
    rig.desk.rename(HWND, &[9], "Card number");
    let s = fresh_bind(&rig, &plan, 3);
    assert_eq!(s.classification().tier, Tier::Never);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::ActionRefused)
    );
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    assert!(rig
        .events
        .kinds()
        .contains(&AgentEventKind::TaskFinished { outcome: "refused" }));
    // The task is over: nothing more can be minted, even with the page clean again.
    rig.desk.rename(HWND, &[9], "Notes");
    let s = fresh_bind(&rig, &plan, 3);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
}

#[test]
fn cond_step_grant_required_for_non_batchable_consequential() {
    let (rig, plan, mut grant) = live5();
    for i in 0..3 {
        rig.run_step(&plan, &mut grant, i);
    }
    let s = fresh_bind(&rig, &plan, 3);
    assert!(matches!(
        rig.gate.evaluate(&s, Some(&grant)),
        Evaluation::NeedsApproval
    ));
    // The refusal consumes nothing; take the SAME bound step to the user.
    let g = rig.step_grant(&s);
    assert_eq!(
        g.action_digest(),
        rig.gate
            .action_digest_of(plan_task(&rig), &grant.plan_digest(), &s)
            .unwrap()
    );
    let a = rig.gate.authorize(s, &mut grant, Some(g)).unwrap();
    assert_eq!(a.step().index(), 3);
    rig.gate.record_result(a.digest(), &ExecResult::executed());
}

fn plan_task(rig: &Rig) -> TaskId {
    rig.gate.current_task().expect("task")
}

#[test]
fn without_the_step_grant_authorize_asks_for_one() {
    let (rig, plan, mut grant) = live5();
    for i in 0..3 {
        rig.run_step(&plan, &mut grant, i);
    }
    let s = fresh_bind(&rig, &plan, 3);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::StepApprovalRequired)
    );
}

#[test]
fn step_grant_for_another_step_or_a_tampered_step_is_refused() {
    let (rig, plan, mut grant) = live5();
    for i in 0..3 {
        rig.run_step(&plan, &mut grant, i);
    }
    // Approved: Refresh as bound to observation O1.
    let s1 = fresh_bind(&rig, &plan, 3);
    let g = rig.step_grant(&s1);
    // The same step re-bound to a new observation (a new element ref) is a different action.
    rig.advance(300);
    let s1b = rig.bind(&plan, 3);
    assert_eq!(
        rig.gate.authorize(s1b, &mut grant, Some(g)).err(),
        Some(Refusal::StepGrantMismatch)
    );
    // A grant for step 3 cannot run step 4 (the submit) either.
    let s3 = fresh_bind(&rig, &plan, 3);
    let g3 = rig.step_grant(&s3);
    let a = rig.gate.authorize(s3, &mut grant, Some(g3)).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    let s4 = fresh_bind(&rig, &plan, 4);
    let g4 = rig.step_grant(&s4);
    let s4b = fresh_bind(&rig, &plan, 4);
    // Submit grant offered for a re-bound submit: mismatch.
    assert_eq!(
        rig.gate.authorize(s4b, &mut grant, Some(g4)).err(),
        Some(Refusal::StepGrantMismatch)
    );
}

#[test]
fn step_grant_digest_covers_text_target_and_rect() {
    let rig = Rig::new();
    let nonce = [7u8; 32];
    let obs = rig.observe();
    let mk = |step: &str, i: u16| {
        let plan = rig.propose(&steps_json(&[step]));
        let b = bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, i)).unwrap();
        let d = action::action_digest(
            &nonce,
            1,
            &[0u8; 32],
            0,
            b.action().unwrap(),
            b.descriptor(),
        );
        (b, d)
    };
    let (_, a) = mk(&type_into("Name", "John Smith"), 0);
    let (_, same) = mk(&type_into("Name", "John Smith"), 0);
    let (_, text) = mk(&type_into("Name", "John Smyth"), 0);
    let (_, target) = mk(&type_into("Notes", "John Smith"), 0);
    assert_eq!(a, same);
    assert_ne!(a, text);
    assert_ne!(a, target);
    rig.desk
        .move_rect(HWND, &[1], frame::Rect::new(205, 200, 300, 30));
    let obs2 = rig.observe();
    let plan = rig.propose(&steps_json(&[&type_into("Name", "John Smith")]));
    let b = bind_step(&plan.steps()[0], &obs2, &rig.ctx(&obs2, 0)).unwrap();
    let moved = action::action_digest(
        &nonce,
        1,
        &[0u8; 32],
        0,
        b.action().unwrap(),
        b.descriptor(),
    );
    assert_ne!(a, moved);
}

#[test]
fn cond_kill_epoch_must_be_unchanged() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    assert!(a.check_live().is_ok());
    rig.kill.bump_silently();
    assert!(a.check_live().is_err());
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    let s = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Killed)
    );
    assert_eq!(rig.gate.mode(), Mode::Observe);
}

#[test]
fn cond_gate_voided_invalidates_grants_and_actions() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.void.fetch_add(1, Ordering::SeqCst);
    assert!(a.check_live().is_err());
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    let s = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Voided)
    );
}

#[test]
fn action_expires_after_two_seconds() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.advance(1999);
    assert!(a.check_live().is_ok());
    rig.advance(2);
    assert!(a.check_live().is_err());
}

#[test]
fn cond_audit_intent_must_succeed() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.audit.fail_intents(true);
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::AuditFailed)
    );
    assert_eq!(grant.next_step(), 1, "nothing advanced");
    assert!(rig.audit.intents().is_empty());
    // The failure released the busy token and spent nothing: after the sink recovers it works.
    rig.audit.fail_intents(false);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    assert_eq!(rig.audit.intents().len(), 1);
    rig.gate.record_result(a.digest(), &ExecResult::executed());
}

#[test]
fn cond_single_busy_token_while_an_action_is_outstanding() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    let s2 = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s2, &mut grant, None).err(),
        Some(Refusal::Busy)
    );
    // An unknown digest does not release it; the right one does.
    rig.gate
        .record_result(ActionDigest::new([9; 32]), &ExecResult::executed());
    let s2 = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s2, &mut grant, None).err(),
        Some(Refusal::Busy)
    );
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    let s2 = fresh_bind(&rig, &plan, 2);
    let a2 = rig.gate.authorize(s2, &mut grant, None).unwrap();
    rig.gate.record_result(a2.digest(), &ExecResult::executed());
}

#[test]
fn cond_single_busy_token_while_an_approval_is_pending() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.run_step(&plan, &mut grant, 1);
    let s = fresh_bind(&rig, &plan, 2);
    let pending = rig.gate.request_step_approval(&s).unwrap();
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Busy)
    );
    // A second approval cannot be requested either.
    let s = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.request_step_approval(&s).err(),
        Some(GateError::Busy)
    );
    // Dropping the pending approval cancels it and frees the token.
    drop(pending);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
}

#[test]
fn cond_rate_floor_250ms_and_30_per_minute() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.advance(300);
    let s = rig.bind(&plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    rig.advance(100);
    let s = rig.bind(&plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::RateLimited)
    );
    rig.advance(151);
    let s = rig.bind(&plan, 2);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());

    // 30 mints in the last minute: the 31st is refused even with the gap respected.
    let now = rig.gate.clock().now();
    {
        let mut g = rig.gate.lock();
        g.mints.clear();
        for i in 0..30u64 {
            g.mints
                .push_back(now - Duration::from_secs(1) - Duration::from_millis(i));
        }
        g.last_mint = Some(now - Duration::from_secs(1));
    }
    rig.advance(300);
    let s = rig.bind(&plan, 3);
    let g3 = rig.step_grant(&s);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, Some(g3)).err(),
        Some(Refusal::RateLimited)
    );
    rig.advance(60_000);
    // (grants and the task have expired by now; the rate window itself is what we checked)
}

#[test]
fn cond_plan_grant_expires_after_ten_minutes() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    {
        let mut g = rig.gate.lock();
        let f = g.task.as_mut().unwrap().frozen.as_mut().unwrap();
        f.approved_at = rig.gate.clock().now() - PLAN_GRANT_TTL;
    }
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Expired)
    );
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig
        .events
        .kinds()
        .contains(&AgentEventKind::TaskFinished { outcome: "expired" }));
}

#[test]
fn cond_plan_grant_expires_after_thirty_seconds_without_progress() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.advance(29_000);
    let s = rig.bind(&plan, 1);
    // Just inside the window.
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    rig.advance(30_000);
    let s = rig.bind(&plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Expired)
    );
    assert!(rig.gate.current_task().is_none());
}

// ---- reads, skips, non-actions --------------------------------------------------------

#[test]
fn read_ops_complete_without_a_mint_and_cannot_be_authorized() {
    let (rig, plan, mut grant) = live5();
    let s = fresh_bind(&rig, &plan, 0);
    assert!(matches!(
        rig.gate.evaluate(&s, Some(&grant)),
        Evaluation::Allow
    ));
    let s2 = fresh_bind(&rig, &plan, 0);
    assert_eq!(
        rig.gate.authorize(s2, &mut grant, None).err(),
        Some(Refusal::NotAnAction)
    );
    rig.gate.complete_read(&s, &mut grant).unwrap_or_else(|_| {
        // `s` was bound before `s2` re-noted the observation: bind again.
        let s = fresh_bind(&rig, &plan, 0);
        rig.gate.complete_read(&s, &mut grant).unwrap();
    });
    assert_eq!(grant.next_step(), 1);
    assert!(rig.audit.intents().is_empty());
    // complete_read refuses a mutating step.
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.complete_read(&s, &mut grant).err(),
        Some(Refusal::NotARead)
    );
}

#[test]
fn plan_time_never_steps_are_skipped_not_executed() {
    let rig = Rig::new();
    let json = steps_json(&[&click("Delete"), &click("Notes")]);
    let (plan, mut grant) = rig.go_live(&json);
    assert_eq!(
        rig.gate.skip_refused(1, &mut grant).err(),
        Some(Refusal::WrongStepIndex)
    );
    rig.gate.skip_refused(0, &mut grant).unwrap();
    assert_eq!(grant.next_step(), 1);
    // Step 1 was not refused at plan time.
    assert_eq!(
        rig.gate.skip_refused(1, &mut grant).err(),
        Some(Refusal::NotRefused)
    );
    rig.run_step(&plan, &mut grant, 1);
    assert!(rig.events.kinds().contains(&AgentEventKind::TaskFinished {
        outcome: "completed"
    }));
}

#[test]
fn unknown_chords_never_become_actions() {
    let rig = Rig::new();
    let json = steps_json(&[r#"{"op":"key_combo","keys":["ctrl","v"]}"#]);
    let (plan, mut grant) = rig.go_live(&json);
    let s = fresh_bind(&rig, &plan, 0);
    assert!(s.action().is_none());
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::NotAnAction)
    );
    let s = fresh_bind(&rig, &plan, 0);
    assert!(matches!(
        rig.gate.request_step_approval(&s).err(),
        Some(GateError::NotAnAction)
    ));
}

// ---- unattended origins -----------------------------------------------------------------

#[test]
fn background_tasks_pause_at_the_first_consequential_step() {
    let rig = Rig::with_origin(TaskOrigin::Background);
    let (plan, mut grant) = rig.go_live(&plan5());
    rig.run_step(&plan, &mut grant, 0);
    rig.run_step(&plan, &mut grant, 1); // Reversible: fine
    let s = fresh_bind(&rig, &plan, 2); // type: Consequential
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::UnattendedConsequential)
    );
    assert!(rig.events.kinds().iter().any(|k| matches!(
        k,
        AgentEventKind::TaskPaused {
            reason: "unattended_consequential"
        }
    )));
    // Even a step grant cannot resume it, and the task stays paused.
    let s = fresh_bind(&rig, &plan, 2);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Paused)
    );
    assert!(rig.audit.intents().len() == 1);
}

#[test]
fn scheduled_tasks_are_read_only() {
    let rig = Rig::with_origin(TaskOrigin::Scheduled);
    let (plan, mut grant) = rig.go_live(&plan5());
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1); // Reversible, still refused
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::ScheduledReadOnly)
    );
    assert!(rig.audit.intents().is_empty());
}

#[test]
fn the_stricter_origin_wins() {
    let rig = Rig::new();
    let cat = catalogue();
    let p = rig
        .gate
        .propose_as(
            TaskId(77),
            TaskOrigin::Scheduled,
            &steps_json(&[OBSERVE]),
            &PlanContext::new(TRANSCRIPT, &cat),
        )
        .unwrap();
    assert_eq!(p.origin(), TaskOrigin::Scheduled);
    let rig = Rig::with_origin(TaskOrigin::Scheduled);
    let p = rig
        .gate
        .propose_as(
            TaskId(78),
            TaskOrigin::Interactive,
            &steps_json(&[OBSERVE]),
            &PlanContext::new(TRANSCRIPT, &cat),
        )
        .unwrap();
    assert_eq!(p.origin(), TaskOrigin::Scheduled);
}

// ---- drift matrix ---------------------------------------------------------------------------

/// Plan `[type Name, type Notes, click Refresh]`-style helpers: dry-run on the pristine desktop,
/// mutate, then bind the step and report what `authorize` says without a step grant.
fn drift_case(
    json: &str,
    mutate: impl Fn(&Rig),
    idx_to_run: usize,
) -> (Rig, Refusal, Option<Refusal>) {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(json);
    for i in 0..idx_to_run {
        rig.run_step(&plan, &mut grant, i);
    }
    mutate(&rig);
    rig.advance(300);
    let s = rig.bind(&plan, idx_to_run);
    let first = match rig.gate.authorize(s, &mut grant, None) {
        Err(e) => e,
        Ok(_) => panic!("expected a refusal"),
    };
    // Whatever the first answer, a StepGrant for the very same bound step settles it (unless the
    // task ended).
    let second = if rig.gate.current_task().is_some() && first == Refusal::DriftPause {
        rig.advance(300);
        let s = rig.bind(&plan, idx_to_run);
        let g = rig.step_grant(&s);
        rig.gate.authorize(s, &mut grant, Some(g)).err()
    } else {
        None
    };
    (rig, first, second)
}

fn type_json() -> String {
    steps_json(&[&type_into("Name", "John Smith")])
}

#[test]
fn no_drift_means_no_pause_for_batchable_steps() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&type_json());
    rig.run_step(&plan, &mut grant, 0);
}

#[test]
fn drift_matrix_each_kind_pauses_into_a_single_step_approval() {
    type Mutation = Box<dyn Fn(&Rig)>;
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "label changed (case only)",
            Box::new(|r| r.desk.rename(HWND, &[1], "NAME")),
        ),
        (
            "role changed",
            Box::new(|r| {
                r.desk.replace_element(
                    HWND,
                    &[1],
                    firewall::fake::ElemSpec::new(
                        1,
                        firewall::NodeRole::ComboBox,
                        "Name",
                        frame::Rect::new(200, 200, 300, 30),
                    )
                    .editable(true),
                )
            }),
        ),
        (
            "rect moved 25 px",
            Box::new(|r| {
                r.desk
                    .move_rect(HWND, &[1], frame::Rect::new(225, 200, 300, 30))
            }),
        ),
        (
            "rect moved 25 px vertically",
            Box::new(|r| {
                r.desk
                    .move_rect(HWND, &[1], frame::Rect::new(200, 175, 300, 30))
            }),
        ),
        (
            "rect resized more than 25 percent",
            Box::new(|r| {
                r.desk
                    .move_rect(HWND, &[1], frame::Rect::new(200, 200, 376, 30))
            }),
        ),
        (
            "different foreground app",
            Box::new(|r| {
                // Same labels, other app, in the foreground.
                let _ = r
                    .desk
                    .clone()
                    .with_window(window_with(0x200, 5000, "other.exe"));
                r.desk.set_foreground(0x200);
            }),
        ),
    ];
    for (name, mutate) in cases {
        let (_rig, first, second) = drift_case(&type_json(), &*mutate, 0);
        assert_eq!(first, Refusal::DriftPause, "{name}");
        assert_eq!(second, None, "{name}: step grant settles it");
        // The approval asked for said "changed since you approved".
        assert!(
            _rig.events
                .kinds()
                .iter()
                .any(|k| matches!(k, AgentEventKind::ApprovalRequested { changed: true, .. })),
            "{name}"
        );
    }
    // The element ref: "Notes" is #8; removing "Save" renumbers it to #7.
    let (_rig, first, second) = drift_case(
        &steps_json(&[&type_into("Notes", "John Smith")]),
        |r| r.desk.remove_element(HWND, &[3]),
        0,
    );
    assert_eq!(first, Refusal::DriftPause, "element ref");
    assert_eq!(second, None);
}

#[test]
fn editable_flag_changing_is_drift_and_raises_the_tier() {
    let combo = |editable: bool| {
        firewall::fake::ElemSpec::new(
            9,
            firewall::NodeRole::ComboBox,
            "Notes",
            frame::Rect::new(200, 500, 300, 30),
        )
        .editable(editable)
    };
    let rig = Rig::new();
    rig.desk.replace_element(HWND, &[9], combo(true));
    // An editable combo box is a text entry: clicking it only focuses (Reversible).
    let (plan, mut grant) = rig.go_live(&steps_json(&[&click("Notes")]));
    let s = rig.bind(&plan, 0);
    assert_eq!(s.classification().tier, Tier::Reversible);
    rig.desk.replace_element(HWND, &[9], combo(false));
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    assert_eq!(s.classification().tier, Tier::Consequential);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::DriftPause)
    );
}

#[test]
fn small_moves_are_not_drift() {
    for rect in [
        frame::Rect::new(224, 200, 300, 30),
        frame::Rect::new(200, 176, 300, 30),
        frame::Rect::new(200, 200, 375, 30),
        frame::Rect::new(200, 200, 300, 37),
    ] {
        let rig = Rig::new();
        let (plan, mut grant) = rig.go_live(&type_json());
        rig.desk.move_rect(HWND, &[1], rect);
        rig.advance(300);
        let s = rig.bind(&plan, 0);
        let a = rig.gate.authorize(s, &mut grant, None).expect("no drift");
        rig.gate.record_result(a.digest(), &ExecResult::executed());
    }
}

#[test]
fn payment_flag_appearing_aborts_the_task() {
    let (rig, first, _) = drift_case(
        &type_json(),
        |r| r.desk.rename(HWND, &[9], "Card number"),
        0,
    );
    assert_eq!(first, Refusal::ActionRefused);
    assert!(rig.gate.current_task().is_none());
}

#[test]
fn a_planned_open_app_exempts_the_app_change() {
    let rig = Rig::new();
    let json = steps_json(&[OPEN_NOTEPAD, &type_into("Name", "John Smith")]);
    let (plan, mut grant) = rig.go_live(&json);
    rig.run_step(&plan, &mut grant, 0);
    // The launch switched the foreground to another app showing the same field.
    let d = rig.desk.clone();
    let _ = d.clone().with_window(window_with(0x200, 5000, "other.exe"));
    d.set_foreground(0x200);
    rig.advance(300);
    let s = rig.bind(&plan, 1);
    let a = rig
        .gate
        .authorize(s, &mut grant, None)
        .expect("app change right after open_app is not drift");
    rig.gate.record_result(a.digest(), &ExecResult::executed());

    // Without the planned open_app in front, the same change IS drift.
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&steps_json(&[&type_into("Name", "John Smith")]));
    let d = rig.desk.clone();
    let _ = d.clone().with_window(window_with(0x200, 5000, "other.exe"));
    d.set_foreground(0x200);
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::DriftPause)
    );
    // S2-22b (F-2): the step approval carries the gate's own drift verdict.
    let s = rig.bind(&plan, 0);
    let pending = rig.gate.request_step_approval(&s).unwrap();
    assert!(pending.changed(), "an app change is reported as changed");
}

#[test]
fn an_undrifted_asked_again_step_is_not_flagged_changed() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.run_step(&plan, &mut grant, 1);
    // Step 2 asks again by its tier (see the pending-approval test above), with no drift.
    let s = fresh_bind(&rig, &plan, 2);
    let pending = rig.gate.request_step_approval(&s).unwrap();
    assert!(!pending.changed());
}

#[test]
fn steps_that_could_not_be_bound_in_the_dry_run_are_asked_again() {
    let rig = Rig::new();
    // "Late" appears only later.
    let json = steps_json(&[&type_into("Late", "John Smith")]);
    let plan = rig.propose(&json);
    let report = rig.dry(&plan);
    assert_eq!(report.unbound(), 1);
    let mut grant = rig.approve_with(&plan, &report);
    rig.desk.replace_element(
        HWND,
        &[9],
        firewall::fake::ElemSpec::new(
            9,
            firewall::NodeRole::TextField,
            "Late",
            frame::Rect::new(200, 500, 300, 30),
        )
        .editable(true),
    );
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    // A batchable type step, but its exact target was never shown: single-step approval.
    assert_eq!(
        rig.gate.evaluate(&s, Some(&grant)),
        Evaluation::NeedsApproval
    );
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::DriftPause)
    );
}

// ---- misc ---------------------------------------------------------------------------------

#[test]
fn audit_and_debug_never_contain_typed_text_or_hash() {
    let rig = Rig::new();
    let marker = "PRIVATE_MARKER_91";
    let json = steps_json(&[&type_into("Name", marker)]);
    let (plan, mut grant) = rig.go_live(&json);
    let s = fresh_bind(&rig, &plan, 0);
    let g = rig.step_grant(&s);
    let a = rig.gate.authorize(s, &mut grant, Some(g)).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    let all = rig.audit.all();
    assert!(all.iter().any(|r| r.event == "intent"), "log alive");
    for r in &all {
        let dump = format!(
            "{r:?} {} {:?}",
            r.target_label.clone().unwrap_or_default(),
            r.app_id
        );
        assert!(!dump.contains(marker), "{dump}");
        assert!(!dump.to_lowercase().contains("private_marker"));
    }
    let intent = rig.audit.intents().remove(0);
    assert_eq!(intent.text_len, Some(marker.chars().count() as u32));
    assert_eq!(intent.op, Some("type_text"));
    assert_eq!(intent.target_role, Some("text_field"));
    assert_eq!(intent.target_label.as_deref(), Some("Name"));
    assert_eq!(intent.app_id.as_deref(), Some("notes.exe"));
    let dbg = format!("{a:?} {:?} {:?}", rig.events.events(), grant);
    assert!(!dbg.contains(marker));
}

// ---- round 2: reservation identity, busy-token lifetime, poison, panics -----------------------

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;

use crate::audit::MemoryAudit;
use crate::facts::PolicyContext;
use crate::never_apps::NeverApps;

/// An audit sink whose `intent` can be made to block (per call, in order) or panic.
struct HookAudit {
    mem: Arc<MemoryAudit>,
    stops: Mutex<VecDeque<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    panic_next: Arc<AtomicBool>,
}

impl AuditSink for HookAudit {
    fn intent(&self, r: &AuditRecord) -> Result<(), crate::audit::AuditError> {
        if self.panic_next.swap(false, Ordering::SeqCst) {
            panic!("sink exploded");
        }
        let stop = self.stops.lock().unwrap().pop_front();
        if let Some((entered, release)) = stop {
            entered.send(()).ok();
            release.recv().ok();
        }
        self.mem.intent(r)
    }
    fn record(&self, r: &AuditRecord) {
        self.mem.record(r);
    }
    fn killed(&self, r: &AuditRecord) {
        self.mem.killed(r);
    }
}

struct Ctl {
    /// Fires when the n-th `intent` call has been entered (and is now blocked).
    entered: Vec<mpsc::Receiver<()>>,
    /// Lets the n-th blocked `intent` call return.
    release: Vec<mpsc::Sender<()>>,
    panic_next: Arc<AtomicBool>,
}

/// A rig whose first `blocking` intent calls block until released.
fn hooked(blocking: usize) -> (Rig, Ctl) {
    let mut stops = VecDeque::new();
    let (mut entered, mut release) = (Vec::new(), Vec::new());
    for _ in 0..blocking {
        let (etx, erx) = mpsc::channel();
        let (rtx, rrx) = mpsc::channel();
        stops.push_back((etx, rrx));
        entered.push(erx);
        release.push(rtx);
    }
    let panic_next = Arc::new(AtomicBool::new(false));
    let pn = panic_next.clone();
    let rig = Rig::with_sink(PolicyConfig::default(), move |mem| {
        Arc::new(HookAudit {
            mem,
            stops: Mutex::new(stops),
            panic_next: pn,
        })
    });
    (
        rig,
        Ctl {
            entered,
            release,
            panic_next,
        },
    )
}

type AuthResult = (Result<ActionDigest, Refusal>, PlanGrant);

fn spawn_authorize(
    gate: &Arc<PolicyGate>,
    step: BoundStep,
    mut grant: PlanGrant,
) -> thread::JoinHandle<AuthResult> {
    let gate = Arc::clone(gate);
    thread::spawn(move || {
        let r = gate.authorize(step, &mut grant, None).map(|a| a.digest());
        (r, grant)
    })
}

fn busy_of(rig: &Rig) -> Busy {
    rig.gate.lock().busy
}

fn frozen_next(rig: &Rig) -> u16 {
    let g = rig.gate.lock();
    g.task.as_ref().unwrap().frozen.as_ref().unwrap().next_step
}

#[test]
fn a_superseded_task_cannot_mint_against_the_new_task() {
    let (rig, ctl) = hooked(1);
    let (plan1, mut grant1) = rig.go_live(&plan5());
    let task1 = plan_task(&rig);
    rig.run_step(&plan1, &mut grant1, 0);
    let s1 = fresh_bind(&rig, &plan1, 1);
    let h1 = spawn_authorize(&rig.gate, s1, grant1);
    ctl.entered[0].recv().unwrap();
    assert!(matches!(busy_of(&rig), Busy::Reserving(_)));

    // Task 2 supersedes task 1 while task 1's audit append is stalled.
    let plan2 = rig.propose(&plan5());
    let task2 = plan_task(&rig);
    assert_ne!(task1, task2);
    // Ending task 1 did not release its busy token: task 2 cannot go Live (or mint) beside it.
    assert!(matches!(busy_of(&rig), Busy::Reserving(_)));
    let report = rig.dry(&plan2);
    assert_eq!(
        rig.gate.request_plan_approval(&plan2, &report).err(),
        Some(GateError::Busy)
    );

    ctl.release[0].send(()).unwrap();
    let (r1, grant1) = h1.join().unwrap();
    assert_eq!(r1.err(), Some(Refusal::Voided), "task 1 must be refused");
    assert_eq!(grant1.next_step(), 1, "task 1's grant did not advance");
    assert_eq!(busy_of(&rig), Busy::Idle);
    assert_eq!(rig.gate.current_task(), Some(task2));
    assert_eq!(rig.gate.mode(), Mode::Observe, "task 2 untouched");
    // Task 2 proceeds normally.
    let mut grant2 = rig.approve_with(&plan2, &report);
    rig.run_step(&plan2, &mut grant2, 0);
    assert_eq!(frozen_next(&rig), 1);
    let s2 = fresh_bind(&rig, &plan2, 1);
    let a = rig.gate.authorize(s2, &mut grant2, None).unwrap();
    assert_eq!(a.task(), task2);
    rig.gate.record_result(a.digest(), &ExecResult::executed());
}

#[test]
fn phase_c_re_validates_the_whole_reservation_identity_even_if_the_token_were_lost() {
    // The reviewer's interleaving: both authorizes blocked in `intent`. The busy token is forced
    // idle by hand (what the old `end_task` did) so that task 2 CAN reserve; task 1 must still be
    // refused in phase C and must not touch task 2's bookkeeping or reservation.
    let (rig, ctl) = hooked(2);
    let (plan1, mut grant1) = rig.go_live(&plan5());
    rig.run_step(&plan1, &mut grant1, 0);
    let s1 = fresh_bind(&rig, &plan1, 1);
    let h1 = spawn_authorize(&rig.gate, s1, grant1);
    ctl.entered[0].recv().unwrap();

    rig.gate.lock().busy = Busy::Idle;
    let (plan2, mut grant2) = rig.go_live(&plan5());
    rig.run_step(&plan2, &mut grant2, 0);
    let s2 = fresh_bind(&rig, &plan2, 1);
    let h2 = spawn_authorize(&rig.gate, s2, grant2);
    ctl.entered[1].recv().unwrap();
    let Busy::Reserving(resv2) = busy_of(&rig) else {
        panic!("task 2 should hold the reservation")
    };

    ctl.release[0].send(()).unwrap();
    let (r1, _) = h1.join().unwrap();
    assert_eq!(r1.err(), Some(Refusal::Voided));
    // Untouched: task 2's next step and its reservation.
    assert_eq!(frozen_next(&rig), 1);
    assert_eq!(busy_of(&rig), Busy::Reserving(resv2));
    // The follow-up record closes task 1's intent.
    let intents = rig.audit.intents();
    assert_eq!(intents.len(), 1);
    let follow: Vec<_> = rig
        .audit
        .records()
        .into_iter()
        .filter(|r| r.event == "refused_after_intent")
        .collect();
    assert_eq!(follow.len(), 1);
    assert_eq!(follow[0].action_digest8, intents[0].action_digest8);
    assert_eq!(follow[0].result_code, Some("voided"));
    assert!(follow[0].target_label.is_none() && follow[0].text_len.is_none());

    ctl.release[1].send(()).unwrap();
    let (r2, grant2) = h2.join().unwrap();
    assert!(r2.is_ok(), "task 2 mints normally");
    assert_eq!(grant2.next_step(), 2);
    assert_eq!(frozen_next(&rig), 2);
}

fn stalled_reservation() -> (Rig, Ctl, ProposedPlan, thread::JoinHandle<AuthResult>) {
    let (rig, ctl) = hooked(1);
    let (plan, mut grant) = rig.go_live(&plan5());
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let h = spawn_authorize(&rig.gate, s, grant);
    ctl.entered[0].recv().unwrap();
    (rig, ctl, plan, h)
}

#[test]
fn phase_c_void_generation_is_checked_on_its_own() {
    let (rig, ctl, _plan, h) = stalled_reservation();
    // The generation moves without void_all running: the task is still live and the token held.
    rig.gate.void.fetch_add(1, Ordering::SeqCst);
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::Voided));
    assert_eq!(grant.next_step(), 1);
    assert_eq!(frozen_next(&rig), 1);
    assert_eq!(busy_of(&rig), Busy::Idle);
}

#[test]
fn phase_c_kill_epoch_is_checked_on_its_own() {
    let (rig, ctl, _plan, h) = stalled_reservation();
    rig.kill.bump_silently();
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::Killed));
    assert_eq!(grant.next_step(), 1);
    assert_eq!(busy_of(&rig), Busy::Idle);
}

#[test]
fn phase_c_frozen_plan_and_step_are_checked_on_their_own() {
    // Same task id and proposal id, but the frozen plan digest differs: refused.
    let (rig, ctl, _plan, h) = stalled_reservation();
    {
        let mut g = rig.gate.lock();
        let f = g.task.as_mut().unwrap().frozen.as_mut().unwrap();
        f.digest = PlanDigest::from_bytes_for_test([7; 32]);
    }
    ctl.release[0].send(()).unwrap();
    assert_eq!(h.join().unwrap().0.err(), Some(Refusal::Voided));
    // Same, with the frozen next step moved.
    let (rig, ctl, _plan, h) = stalled_reservation();
    rig.gate
        .lock()
        .task
        .as_mut()
        .unwrap()
        .frozen
        .as_mut()
        .unwrap()
        .next_step = 3;
    ctl.release[0].send(()).unwrap();
    assert_eq!(h.join().unwrap().0.err(), Some(Refusal::Voided));
    assert_eq!(frozen_next(&rig), 3, "not advanced by the refused call");
}

#[test]
fn phase_c_task_identity_is_checked_on_its_own() {
    // Task id differs (proposal id and everything else the same).
    let (rig, ctl, _plan, h) = stalled_reservation();
    rig.gate.lock().task.as_mut().unwrap().task = TaskId(999);
    ctl.release[0].send(()).unwrap();
    assert_eq!(h.join().unwrap().0.err(), Some(Refusal::Voided));
    // Proposal id differs.
    let (rig, ctl, _plan, h) = stalled_reservation();
    rig.gate.lock().task.as_mut().unwrap().proposal += 100;
    ctl.release[0].send(()).unwrap();
    assert_eq!(h.join().unwrap().0.err(), Some(Refusal::Voided));
}

#[test]
fn ending_the_task_does_not_release_an_outstanding_action() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    let old_task = plan_task(&rig);

    // A new task supersedes the one with the live action.
    let plan2 = rig.propose(&plan5());
    let task2 = plan_task(&rig);
    assert_ne!(task2, old_task);
    assert!(matches!(busy_of(&rig), Busy::Outstanding { .. }));
    // No second action can be authorised beside the live one: the new task cannot even go Live.
    let report = rig.dry(&plan2);
    assert_eq!(
        rig.gate.request_plan_approval(&plan2, &report).err(),
        Some(GateError::Busy)
    );

    // The old task's (failed) result must not touch or end the new task.
    rig.gate
        .record_result(a.digest(), &ExecResult::failed("driver_error"));
    assert_eq!(rig.gate.current_task(), Some(task2));
    assert_eq!(busy_of(&rig), Busy::Idle);
    let mut grant2 = rig.approve_with(&plan2, &report);
    rig.run_step(&plan2, &mut grant2, 0);
    let s2 = fresh_bind(&rig, &plan2, 1);
    let a2 = rig.gate.authorize(s2, &mut grant2, None).unwrap();
    rig.gate.record_result(a2.digest(), &ExecResult::executed());
}

#[test]
fn an_unreported_action_releases_the_busy_token_after_action_ttl() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.advance(1500);
    let s2 = rig.bind(&plan, 2);
    assert_eq!(
        rig.gate.authorize(s2, &mut grant, None).err(),
        Some(Refusal::Busy)
    );
    rig.advance(600);
    assert!(a.check_live().is_err(), "the action is dead past 2 s");
    let s2 = rig.bind(&plan, 2);
    let a2 = rig.gate.authorize(s2, &mut grant, None).unwrap();
    rig.gate.record_result(a2.digest(), &ExecResult::executed());
}

#[test]
fn a_panicking_audit_sink_does_not_wedge_the_gate() {
    let (rig, ctl) = hooked(0);
    let (plan, mut grant) = rig.go_live(&plan5());
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    ctl.panic_next.store(true, Ordering::SeqCst);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rig.gate.authorize(s, &mut grant, None).map(|a| a.digest())
    }));
    assert!(r.is_err(), "the sink's panic propagates");
    assert_eq!(busy_of(&rig), Busy::Idle, "the reservation was released");
    assert_eq!(grant.next_step(), 1);
    assert!(!rig.gate.dead(), "no gate lock was held while the sink ran");
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    rig.gate.record_result(a.digest(), &ExecResult::executed());
}

fn poison(rig: &Rig) {
    let gate = Arc::clone(&rig.gate);
    let r = thread::spawn(move || {
        let _g = gate.inner.lock().unwrap();
        panic!("boom while holding the gate lock");
    })
    .join();
    assert!(r.is_err());
    assert!(rig.gate.inner.is_poisoned());
}

#[test]
fn a_poisoned_gate_lock_fails_closed_everywhere() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&plan5());
    rig.run_step(&plan, &mut grant, 0);
    let s_eval = fresh_bind(&rig, &plan, 1);
    let a_before = {
        let s = fresh_bind(&rig, &plan, 1);
        rig.gate.authorize(s, &mut grant, None).unwrap()
    };
    rig.gate
        .record_result(a_before.digest(), &ExecResult::executed());
    let live = {
        let s = fresh_bind(&rig, &plan, 2);
        let g = rig.step_grant(&s);
        rig.gate.authorize(s, &mut grant, Some(g)).unwrap()
    };
    assert!(live.check_live().is_ok());
    // A waiter blocked on a fresh approval.
    rig.gate
        .record_result(live.digest(), &ExecResult::executed());
    let s3 = fresh_bind(&rig, &plan, 3);
    let pending = rig.gate.request_step_approval(&s3).unwrap();
    let s_edge = rig.bind(&plan, 3);
    let id = pending.id();
    let hex = pending.digest_hex();
    let waiter = thread::spawn(move || pending.wait());
    thread::sleep(Duration::from_millis(30));

    poison(&rig);

    assert_eq!(waiter.join().unwrap().err(), Some(GateError::Voided));
    assert_eq!(
        rig.gate.authorize(s_edge, &mut grant, None).err(),
        Some(Refusal::GatePoisoned)
    );
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::Refused(Refusal::GatePoisoned))
    );
    assert_eq!(
        rig.gate.evaluate(&s_eval, Some(&grant)),
        Evaluation::Deny(Refusal::GatePoisoned)
    );
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    assert_eq!(
        rig.gate.complete_read(&s_eval, &mut grant).err(),
        Some(Refusal::GatePoisoned)
    );
    assert_eq!(
        rig.gate.skip_refused(0, &mut grant).err(),
        Some(Refusal::GatePoisoned)
    );
    assert_eq!(
        rig.gate.request_step_approval(&s_eval).err(),
        Some(GateError::Refused(Refusal::GatePoisoned))
    );
    let cat = crate::world::catalogue();
    assert_eq!(
        rig.gate
            .propose(
                TaskId(77),
                &plan5(),
                &PlanContext::new(crate::world::TRANSCRIPT, &cat)
            )
            .err(),
        Some(PlanError::GateUnavailable)
    );
    assert_eq!(
        rig.gate
            .note_observation(rig.observe().id(), SessionState::Active)
            .err(),
        Some(GateError::Refused(Refusal::GatePoisoned))
    );
    // Actions minted earlier are dead too.
    assert!(live.check_live().is_err());
}

#[test]
fn a_gate_poisoned_during_the_audit_append_refuses_the_commit() {
    let (rig, ctl, _plan, h) = stalled_reservation();
    poison(&rig);
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::GatePoisoned));
    assert_eq!(grant.next_step(), 1);
}

// ---- round 2: stricter wins over a weaker bind context; grant identity ---------------------------

fn strict_rig() -> Rig {
    Rig::with_config(PolicyConfig {
        never_apps: NeverApps::builtin().with_extra("notes"),
        ..PolicyConfig::default()
    })
}

/// Binds step `i` the way a careless caller would: with an EMPTY (builtin-only) policy context.
fn weak_bind(rig: &Rig, plan: &ProposedPlan, i: usize) -> BoundStep {
    let obs = rig.observe();
    rig.gate
        .note_observation(obs.id(), SessionState::Active)
        .unwrap();
    let mut ctx = rig.ctx(&obs, i as u16);
    ctx.policy = PolicyContext::new(NeverApps::builtin());
    bind_step(&plan.steps()[i], &obs, &ctx).unwrap()
}

#[test]
fn the_gates_own_policy_wins_over_a_weaker_bind_context() {
    let rig = strict_rig();
    let plan = rig.propose(&steps_json(&[&click("Notes"), &click("Notes")]));
    // The weak bind sees nothing wrong with the app...
    let s = weak_bind(&rig, &plan, 0);
    assert_eq!(s.classification().tier, Tier::Reversible);
    // ...but the gate classifies with its own list: Never.
    assert!(matches!(
        rig.gate.evaluate(&s, None),
        Evaluation::WouldDo(Tier::Never)
    ));
    // Approve the plan from a weakly-bound dry run (Never is shown, since the gate re-classifies).
    let obs = rig.observe();
    let mut ctx = rig.ctx(&obs, 0);
    ctx.policy = PolicyContext::new(NeverApps::builtin());
    let report = rig.gate.dry_run(&plan, &obs, &ctx);
    let mut grant = rig.approve_with(&plan, &report);
    rig.advance(300);
    let s = weak_bind(&rig, &plan, 0);
    assert_eq!(
        rig.gate.evaluate(&s, Some(&grant)),
        Evaluation::Deny(Refusal::ActionRefused)
    );
    assert_eq!(
        rig.gate.request_step_approval(&s).err(),
        Some(GateError::Refused(Refusal::ActionRefused))
    );
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::ActionRefused)
    );
    assert!(rig.audit.intents().is_empty());
}

#[test]
fn an_old_grant_for_a_re_proposed_task_id_is_refused_by_the_plan_digest() {
    let rig = Rig::new();
    let cat = crate::world::catalogue();
    let pctx = PlanContext::new(crate::world::TRANSCRIPT, &cat);
    let same = TaskId(7);
    let a = rig
        .gate
        .propose(same, &steps_json(&[&click("Notes")]), &pctx)
        .unwrap();
    let mut grant_a = rig.approve(&a);
    // The SAME task id is proposed again with a different plan; the old grant still carries
    // task 7, the same kill epoch and the same void generation.
    let b = rig
        .gate
        .propose(
            same,
            &steps_json(&[&click("Notes"), &click("Refresh")]),
            &pctx,
        )
        .unwrap();
    let _grant_b = rig.approve(&b);
    assert_eq!(grant_a.task(), rig.gate.current_task().unwrap());
    assert_ne!(grant_a.plan_digest(), _grant_b.plan_digest());
    assert_eq!(grant_a.next_step(), _grant_b.next_step());
    rig.advance(300);
    // The re-proposal superseded the first (void generation bumped); resync it by hand so the
    // plan-digest check is the one under test.
    grant_a.void_gen = rig.gate.void.load(Ordering::SeqCst);
    let s = rig.bind(&b, 0);
    assert_eq!(
        rig.gate.authorize(s, &mut grant_a, None).err(),
        Some(Refusal::WrongPlan)
    );
    assert_eq!(
        rig.gate.evaluate(&rig.bind(&b, 0), Some(&grant_a)),
        Evaluation::Deny(Refusal::WrongPlan)
    );
    assert!(rig.audit.intents().is_empty());
}

// ---- round 2: kill epoch in precheck vs the void hook (redundancy documented in precheck) --------

#[test]
fn precheck_task_epoch_and_grant_epoch_each_refuse_on_their_own() {
    // (1) Only the grant's epoch differs from the switch (task epoch moved with it by hand).
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.kill.bump_silently();
    let e = rig.kill.epoch();
    rig.gate.lock().task.as_mut().unwrap().epoch = e;
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Killed)
    );
    // (2) Only the task's epoch differs (grant and switch agree).
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.gate.lock().task.as_mut().unwrap().epoch = 99;
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::Killed)
    );
    // (3) The kill hook (void_all) path: trip() ends the task, so the verdict is TaskEnded/NoTask,
    // not Killed; the epoch checks above are the independent second line of defence.
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    rig.kill.trip(crate::kill::KillReason::Hotkey);
    let s = fresh_bind(&rig, &plan, 1);
    assert_eq!(
        rig.gate.authorize(s, &mut grant, None).err(),
        Some(Refusal::TaskEnded)
    );
}

// ---- S2-13: queued tightenings ---------------------------------------------------------------

#[test]
fn a_superseded_tasks_live_action_dies_at_once() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    assert!(a.check_live().is_ok());
    let _plan2 = rig.propose(&plan5());
    assert!(a.check_live().is_err(), "supersede must revoke the action");
}

#[test]
fn every_end_but_completed_and_exec_failed_bumps_the_void_generation() {
    for (why, bumps) in [
        (EndReason::Completed, false),
        (EndReason::ExecFailed, false),
        (EndReason::Denied, true),
        (EndReason::Timeout, true),
        (EndReason::Expired, true),
        (EndReason::Refused, true),
        (EndReason::Killed, true),
        (EndReason::Voided, true),
        (EndReason::Superseded, true),
    ] {
        let (rig, _plan, _grant) = live5();
        let before = rig.gate.void.load(Ordering::SeqCst);
        let mut out = Out::default();
        {
            let mut g = rig.gate.lock();
            rig.gate.end_task(&mut g, why, Duration::ZERO, &mut out);
        }
        let after = rig.gate.void.load(Ordering::SeqCst);
        assert_eq!(after != before, bumps, "{why:?}");
    }
}

#[test]
fn a_task_ended_by_a_non_void_path_during_phase_b_is_voided_and_does_not_advance() {
    let (rig, ctl, _plan, h) = stalled_reservation();
    // The task expires (a non-void end) while the audit append is stalled.
    {
        let mut out = Out::default();
        let mut g = rig.gate.lock();
        rig.gate
            .end_task(&mut g, EndReason::Expired, Duration::ZERO, &mut out);
    }
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::Voided));
    assert_eq!(grant.next_step(), 1, "no advance");
    assert_eq!(busy_of(&rig), Busy::Idle);
    assert_eq!(rig.audit.intents().len(), 1, "the intent was written ahead");
    assert!(rig
        .audit
        .records()
        .iter()
        .any(|r| r.event == "refused_after_intent"));
}

#[test]
fn a_plan_grant_from_another_gate_is_refused_by_the_gate_check() {
    let (rig, plan, grant) = live5();
    let (other, _oplan, mut ograft) = live5();
    let _ = grant;
    // Same step, same task ids: only the PlanGrant.gate differs.
    let s = fresh_bind(&rig, &plan, 0);
    assert_eq!(
        rig.gate.authorize(s, &mut ograft, None).err(),
        Some(Refusal::WrongPlan)
    );
    assert!(rig.audit.intents().is_empty());
    drop(other);
}

#[test]
fn a_late_result_cannot_end_a_re_proposal_that_reuses_the_task_id() {
    let rig = Rig::new();
    let cat = crate::world::catalogue();
    let pctx = PlanContext::new(crate::world::TRANSCRIPT, &cat);
    let same = TaskId(7);
    let draft = steps_json(&[&click("Notes"), &click("Refresh")]);
    let a = rig.gate.propose(same, &draft, &pctx).unwrap();
    let mut grant_a = rig.approve(&a);
    rig.advance(300);
    let act = rig
        .gate
        .authorize(rig.bind(&a, 0), &mut grant_a, None)
        .unwrap();
    // Re-proposal with the SAME task id supersedes it.
    let b = rig.gate.propose(same, &draft, &pctx).unwrap();
    assert_eq!(rig.gate.current_task(), Some(same));
    rig.gate
        .record_result(act.digest(), &ExecResult::failed("driver_error"));
    assert_eq!(
        rig.gate.current_task(),
        Some(same),
        "the late result of the old proposal must not end the new one"
    );
    drop(b);
}

#[test]
fn an_unmatched_result_leaves_a_content_free_late_result_record() {
    let (rig, plan, mut grant) = live5();
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    let d = a.digest();
    rig.gate.record_result(d, &ExecResult::executed());
    // The same digest again, and a result with nothing outstanding.
    rig.gate.record_result(d, &ExecResult::executed());
    let late: Vec<_> = rig
        .audit
        .records()
        .into_iter()
        .filter(|r| r.event == "late_result")
        .collect();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].action_digest8.as_deref(), Some(d.short().as_str()));
    assert!(late[0].target_label.is_none() && late[0].app_id.is_none());
    assert_eq!(late[0].result_code, Some("ok"));
}

#[test]
fn a_hung_audit_sink_beyond_the_reserve_cap_poisons_the_reservation() {
    let (rig, ctl, plan, h) = stalled_reservation();
    rig.advance(10_001);
    // Any entry point sweeps: the stale reservation is dropped and the gate is Idle again.
    assert_eq!(rig.gate.activity().busy_state, BusyState::Idle);
    let s = rig.bind(&plan, 1);
    let _ = rig.gate.evaluate(&s, None);
    {
        let mut out = Out::default();
        let mut g = rig.gate.lock();
        rig.gate.sweep(&mut g, rig.gate.clock.now(), &mut out);
        assert_eq!(g.busy, Busy::Idle);
    }
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::Voided), "the late mint is refused");
    assert_eq!(grant.next_step(), 1);
    assert_eq!(busy_of(&rig), Busy::Idle);
    assert!(rig
        .audit
        .records()
        .iter()
        .any(|r| r.event == "reservation_expired"));
}

#[test]
fn a_late_phase_c_past_the_cap_is_refused_even_if_nobody_swept() {
    let (rig, ctl, _plan, h) = stalled_reservation();
    rig.advance(10_001);
    ctl.release[0].send(()).unwrap();
    let (r, grant) = h.join().unwrap();
    assert_eq!(r.err(), Some(Refusal::Voided));
    assert_eq!(grant.next_step(), 1);
    assert_eq!(busy_of(&rig), Busy::Idle);
}

#[test]
fn activity_reports_task_and_busy_state() {
    let rig = Rig::new();
    assert!(rig.gate.activity().is_idle());
    let (plan, mut grant) = rig.go_live(&plan5());
    assert!(rig.gate.activity().task_live);
    rig.run_step(&plan, &mut grant, 0);
    let s = fresh_bind(&rig, &plan, 1);
    let a = rig.gate.authorize(s, &mut grant, None).unwrap();
    assert_eq!(rig.gate.activity().busy_state, BusyState::Outstanding);
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    assert_eq!(rig.gate.activity().busy_state, BusyState::Idle);
    rig.gate.void_all(VoidReason::Quit);
    assert!(rig.gate.activity().is_idle());
}
