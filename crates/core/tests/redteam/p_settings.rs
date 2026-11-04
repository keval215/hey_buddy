//! Case p: settings abuse.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::rig::*;
use buddy_core::agent::{RefuseReason, Stage, TaskOutcome};
use buddy_core::api::SettingsPatch;
use driver::fake::CallKind;

/// Attack: the agent is disabled (or the activity probe is missing). Expected:
/// `Refused(agent_disabled | no_activity_probe)`, no engine made, nothing executed.
/// Defence: step 1 of `run_task`.
#[tokio::test]
async fn a_disabled_agent_runs_nothing() {
    let s = Flip::new();
    s.enabled.store(false, Ordering::SeqCst);
    let e = env_with(Opts {
        settings: s,
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::AgentDisabled));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 0);
    e.executed_nothing();

    let s = Flip::new();
    s.probe.store(false, Ordering::SeqCst);
    let e = env_with(Opts {
        settings: s,
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::NoActivityProbe)
    );
    e.executed_nothing();
}

/// Attack: `observe_mode` is flipped OFF while the "Run it for real?" question is open (hoping to
/// skip the approval). Expected: the question stays open and needs an answer; with a "no" nothing
/// runs. Defence: observe mode is read ONCE per task, before the question; the question is the
/// gate's plan approval, not a setting.
#[tokio::test]
async fn flipping_observe_mode_mid_task_does_not_skip_the_approval() {
    let s = Flip::new();
    let e = env_with(Opts {
        settings: s.clone(),
        ..Opts::default()
    });
    let s2 = s.clone();
    e.human.script(&[custom(move |cx| {
        s2.observe.store(false, Ordering::SeqCst);
        cx.deny().unwrap();
    })]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    assert_eq!(e.human.seen().len(), 1);
}

/// Attack: with `observe_mode` OFF the user is not asked "Run it for real?", but the plan
/// approval is still required, and a flip back ON mid-task changes nothing already granted.
/// Expected: a plain plan approval (denied here): nothing runs.
#[tokio::test]
async fn observe_mode_off_still_needs_the_plan_approval() {
    let s = Flip::new();
    s.observe.store(false, Ordering::SeqCst);
    let e = env_with(Opts {
        settings: s,
        ..Opts::default()
    });
    e.human.script(&[Act::Deny]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::PlanApproval), "{r:?}");
    e.executed_nothing();
}

/// Attack: the agent is switched off while a task runs. Documented behaviour
/// (`docs/supervisor.md`, "agent_enabled is read at task start only"): that task is NOT stopped
/// by the setting (the kill switch is the stop), but the NEXT task is refused. Expected: the
/// running task completes only because the user approved every step; the next request is
/// refused; a kill still stops a running task.
#[tokio::test]
async fn switching_the_agent_off_mid_task_refuses_the_next_task_and_the_kill_switch_still_stops() {
    let s = Flip::new();
    let e = env_with(Opts {
        settings: s.clone(),
        ..Opts::default()
    });
    let s2 = s.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            s2.enabled.store(false, Ordering::SeqCst);
        }
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Completed,
        "approved steps finish: {r:?}"
    );
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::AgentDisabled));
    // Same setting flip, plus a kill: the task stops.
    let s = Flip::new();
    let e = env_with(Opts {
        settings: s.clone(),
        ..Opts::default()
    });
    let (s2, ctl) = (s.clone(), e.ctl.clone());
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            s2.enabled.store(false, Ordering::SeqCst);
            ctl.trip(buddy_core::kill::KillReason::Hotkey);
        }
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(e.fake.chars_sent(), 0);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

fn test_buddy(dir: &tempfile::TempDir) -> Arc<buddy_core::Buddy> {
    Arc::new(buddy_core::Buddy::with_credentials(
        dir.path().join("settings.json"),
        Box::new(capture::fake::FakeCapture::new(
            capture::fake::FakeMode::ExcludeWindows,
        )),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    ))
}

/// Attack: `agent_mode` is switched on without `agent_enabled` (through the settings API).
/// Expected: it is stored as false; the real settings object reports the agent disabled; the
/// default of both is OFF. Defence: `Buddy::update_settings` normalises; the supervisor asks the
/// persisted settings.
#[test]
fn agent_mode_without_agent_enabled_is_not_stored_and_the_defaults_are_off() {
    let dir = tempfile::tempdir().unwrap();
    let b = test_buddy(&dir);
    let s = b.get_settings();
    assert!(!s.agent_enabled && !s.agent_mode, "both default to OFF");
    assert!(s.observe_mode, "observe mode defaults to ON");
    b.update_settings(&SettingsPatch {
        agent_mode: Some(true),
        ..Default::default()
    })
    .unwrap();
    assert!(
        !b.get_settings().agent_mode,
        "agent_mode needs agent_enabled"
    );
    let settings = buddy_core::agent::BuddyAgentSettings::new(b.clone());
    use buddy_core::agent::AgentSettings;
    assert!(!settings.agent_enabled());
    // Enabled in the settings but not ready (no activity log, no stop shortcut): still off.
    b.update_settings(&SettingsPatch {
        agent_enabled: Some(true),
        ..Default::default()
    })
    .unwrap();
    assert!(
        !settings.agent_enabled(),
        "settings alone do not enable the agent"
    );
}
