//! Gate-level attacks that the supervisor hides behind its own flow (the supervisor's test hooks
//! are not public): a frozen plan, a spent / stale observation, the digest bind and the 500 ms
//! guard at the gate, and the origin tier cap. Same REAL `PolicyGate` and firewall, driven
//! directly through the public API.

use crate::rig::*;
use firewall::SessionState;
use policy::{
    Decision, Evaluation, GateError, KillReason, Mode, PolicyConfig, Refusal, TaskOrigin,
};

const UP3: &str = r#"{"op":"scroll","direction":"up","amount":3}"#;

fn two_scrolls() -> String {
    steps_json(&[SCROLL, UP3])
}

/// Attack: the runner (or an injected step) skips ahead, repeats, or reorders frozen steps.
/// Expected: the gate refuses (`WrongStepIndex` / `OutOfPlan`), nothing is minted, no intent.
/// Defence: the plan is frozen at approval; `authorize` takes only the grant's next step.
#[test]
fn a_frozen_plan_cannot_be_reordered_skipped_or_repeated() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&two_scrolls());
    rig.advance(300);
    // Skip ahead.
    let s1 = rig.bind(&plan, 1);
    let r = rig.gate.authorize(s1, &mut grant, None);
    assert!(
        matches!(r.err(), Some(Refusal::WrongStepIndex | Refusal::OutOfPlan)),
        "skipping ahead must be refused"
    );
    assert!(rig.audit.intents().is_empty());
    // The right step runs once ...
    rig.run_step(&plan, &mut grant, 0);
    assert_eq!(rig.audit.intents().len(), 1);
    // ... and cannot be repeated.
    rig.advance(300);
    let again = rig.bind(&plan, 0);
    let r = rig.gate.authorize(again, &mut grant, None);
    assert!(
        matches!(r.err(), Some(Refusal::WrongStepIndex | Refusal::OutOfPlan)),
        "repeating a step must be refused"
    );
    assert_eq!(rig.audit.intents().len(), 1, "no second intent");
}

/// Attack: a step bound to an OLD observation is minted after a newer observation was noted
/// (the page changed in between). Expected: `Stale`; no intent. Defence: an action is bound to
/// the latest observation only.
#[test]
fn a_step_bound_to_an_older_observation_is_stale_once_a_newer_one_is_noted() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&two_scrolls());
    rig.advance(300);
    let old = rig.observe();
    let s = rig.bind_in(&plan, 0, &old);
    let newer = rig.observe();
    rig.gate
        .note_observation(newer.id(), SessionState::Active)
        .unwrap();
    let r = rig.gate.authorize(s, &mut grant, None);
    assert!(r.is_err(), "a stale ref must not be minted");
    assert!(rig.audit.intents().is_empty());
}

/// Attack: observation reuse: one observation authorises one action; the same observation is
/// used for the next step. Expected: refused. Defence: the observation is spent by the mint.
#[test]
fn an_observation_authorises_exactly_one_action() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&two_scrolls());
    rig.advance(300);
    let obs = rig.observe();
    let s0 = rig.bind_in(&plan, 0, &obs);
    let a = rig
        .gate
        .authorize(s0, &mut grant, None)
        .expect("first mint");
    rig.gate
        .record_result(a.digest(), &policy::ExecResult::executed());
    rig.advance(300);
    // Noting the same observation again is refused ...
    assert!(rig
        .gate
        .note_observation(obs.id(), SessionState::Active)
        .is_err());
    // ... and a step bound (purely) to it cannot be minted either.
    let s1 = policy::bind_step(&plan.steps()[1], &obs, &rig.ctx(&obs, 1)).expect("bind is pure");
    assert!(rig.gate.authorize(s1, &mut grant, None).is_err());
    assert_eq!(rig.audit.intents().len(), 1);
}

/// Attack: answers with the wrong digest / after the first answer / inside 500 ms, straight at
/// the gate. Expected: `DigestMismatch` / `UnknownApproval` / `TooEarly`; the approval stays
/// pending until a right answer arrives. Defence: digest bind, single use, 500 ms guard.
#[test]
fn the_gate_binds_the_digest_and_keeps_the_500_ms_guard() {
    let rig = Rig::new();
    let plan = rig.propose(&two_scrolls());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let (id, hex) = (pending.id(), pending.digest_hex());
    rig.advance(499);
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::TooEarly)
    );
    rig.advance(1);
    for bad in [
        "0".repeat(64),
        String::new(),
        hex[..32].to_owned(),
        format!("{hex}0"),
    ] {
        assert_eq!(
            rig.gate.decide(id, &bad, Decision::Approve).err(),
            Some(GateError::DigestMismatch),
            "{bad:?}"
        );
    }
    assert_eq!(rig.gate.mode(), Mode::Observe, "nothing was granted");
    rig.gate.decide(id, &hex, Decision::Approve).unwrap();
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::UnknownApproval)
    );
    let _grant = pending.wait().unwrap();

    // The digest of a finished approval cannot approve the next plan.
    let plan2 = rig.propose(&steps_json(&[SCROLL]));
    let report2 = rig.dry(&plan2);
    let p2 = rig.gate.request_plan_approval(&plan2, &report2).unwrap();
    rig.advance(600);
    assert_ne!(hex, p2.digest_hex());
    assert_eq!(
        rig.gate.decide(p2.id(), &hex, Decision::Approve).err(),
        Some(GateError::DigestMismatch)
    );
}

/// Attack: a scheduled task's step (Reversible) and a background task's Consequential click get
/// a grant (suppose the user approved the plan earlier). Expected: the STEP is refused at the
/// gate (`ScheduledReadOnly` / `UnattendedConsequential`), never minted. Defence: the origin tier
/// cap lives in the gate, independent of any approval.
#[test]
fn the_origin_tier_cap_refuses_steps_even_with_a_plan_grant() {
    for (origin, plan, expect) in [
        (
            TaskOrigin::Scheduled,
            steps_json(&[SCROLL]),
            Refusal::ScheduledReadOnly,
        ),
        (
            TaskOrigin::Background,
            steps_json(&[&click("Refresh")]),
            Refusal::UnattendedConsequential,
        ),
    ] {
        let rig = Rig::with_config(PolicyConfig {
            origin,
            ..PolicyConfig::default()
        });
        let plan = rig.propose(&plan);
        let report = rig.dry(&plan);
        let Ok(pending) = rig.gate.request_plan_approval(&plan, &report) else {
            continue; // refused even earlier: fine, fails closed
        };
        rig.advance(600);
        rig.gate
            .decide(pending.id(), &pending.digest_hex(), Decision::Approve)
            .unwrap();
        let mut grant = pending.wait().expect("granted");
        rig.advance(300);
        let s = rig.bind(&plan, 0);
        match rig.gate.evaluate(&s, Some(&grant)) {
            Evaluation::Deny(r) => assert_eq!(r, expect, "{origin:?}"),
            other => panic!("{origin:?}: the step was not refused: {other:?}"),
        }
        assert!(
            rig.gate.authorize(s, &mut grant, None).is_err(),
            "{origin:?}"
        );
        assert!(rig.audit.intents().is_empty(), "{origin:?}");
    }
}

/// Attack: the kill switch fires, then the held grant is used. Expected: refused, nothing minted.
/// Defence: kill voids every grant; a new plan with new approvals is required.
#[test]
fn a_grant_is_worthless_after_the_kill_switch() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&two_scrolls());
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    rig.kill.trip(KillReason::Hotkey);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
    assert!(rig.audit.intents().is_empty());
    assert_eq!(rig.gate.mode(), Mode::Observe);
}
