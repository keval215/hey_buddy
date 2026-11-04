//! Case l: an engine, an approval or a driver call that never answers.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::rig::*;
use agent::fakes::{Hostile, HostileEngine, Scripted};
use buddy_core::agent::{Stage, TaskOutcome, TaskRequest};
use buddy_core::kill::KillReason;
use driver::fake::{CallKind, Inject};

/// Attack: the engine never answers and ignores `cancel` and `shutdown` (`Hostile::Hang`).
/// Expected: the plan deadline (60 s local) fires, the process is KILLED, the task ends
/// `PlanFailed(engine_timeout)`, the gate and the supervisor are free again, nothing ran.
/// Defence: `ProcessEngine` deadline + kill; a hung engine cannot hold the gate. Virtual time
/// (the runtime clock is paused and auto-advances), so the test takes milliseconds.
#[tokio::test(start_paused = true)]
async fn an_engine_that_never_responds_hits_the_deadline_and_is_killed() {
    let f = ChildFactory::new(HostileEngine::new(Hostile::Hang).into_main(), "{}");
    let e = env_with(Opts {
        factory: Some(f.clone()),
        ..Opts::default()
    });
    let r = tokio::time::timeout(
        Duration::from_secs(600),
        e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)),
    )
    .await
    .expect("the deadline must end the task");
    assert_eq!(
        r.outcome,
        TaskOutcome::PlanFailed("engine_timeout"),
        "{r:?}"
    );
    assert!(
        f.rec.kills.load(Ordering::SeqCst) >= 1,
        "the engine was killed"
    );
    e.executed_nothing();
    assert!(e.human.seen().is_empty());
    e.gate_idle();
}

/// Attack: the same hung engine, stopped by the kill switch instead. Expected: `Killed(planning)`
/// promptly, the engine killed, the gate released, and the NEXT task runs normally.
/// Defence: kill is wired to the engine killer; the run loop aborts; the busy flag is dropped on
/// every path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_engine_is_killed_by_the_kill_switch_and_the_gate_is_released() {
    let f = ChildFactory::new(HostileEngine::new(Hostile::Hang).into_main(), "{}");
    let e = env_with(Opts {
        factory: Some(f.clone()),
        ..Opts::default()
    });
    let sup = e.sup.clone();
    let task =
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    wait_until(|| f.rec.spawns.load(Ordering::SeqCst) == 1 && f.rec.plan_tasks() == 1);
    let t = std::time::Instant::now();
    e.ctl.trip(KillReason::Hotkey);
    let r = bounded(task).await.unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert!(f.rec.kills.load(Ordering::SeqCst) >= 1);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.executed_nothing();
    e.gate_idle();
    assert!(!e.sup.is_running(), "the busy flag was dropped");
}

/// Attack: an approval that never arrives (nobody is there). Expected: it expires
/// (`Expired(live_offer)`), nothing runs, no pending request is left, the gate is free and the
/// next task is possible. Defence: approvals expire on the gate's clock.
#[tokio::test]
async fn an_approval_that_never_arrives_expires_and_frees_everything() {
    let e = env();
    e.human.script(&[Act::Silent]);
    let t = e.advance_when_pending(Duration::from_secs(130));
    let r = e.run(&notepad_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Expired(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    assert_eq!(e.broker.pending_count(), 0);
    e.gate_idle();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
}

/// Attack: a scripted engine that hangs forever inside `plan` while a second request arrives.
/// Expected: the second is `busy`; after the kill the first ends `Killed(planning)` and a third
/// request runs. Defence: one task at a time and release on every exit path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_plan_does_not_leak_the_busy_flag() {
    let e = env();
    e.factory.push(Scripted::hang());
    let sup = e.sup.clone();
    let task =
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    let f = e.factory.clone();
    wait_until(|| f.plans() == 1);
    e.ctl.trip(KillReason::StopButton);
    let r = bounded(task).await.unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
}

/// Attack: the driver call hangs (the target app froze mid-typing). Expected: the call deadline
/// ends the step with a typed `Failed`/`Blocked`, the task ends, the gate is released; later
/// resuming the hung call types nothing more. Defence: per-call deadline in the driver; the
/// kill switch is checked between chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_driver_call_ends_the_step_by_deadline() {
    let e = env();
    e.fake.inject(CallKind::SendChars, Inject::Hang);
    let r = e
        .run(&steps_json(&[&type_into("Name", "John Smith")]))
        .await;
    assert!(
        matches!(
            r.outcome,
            TaskOutcome::Failed(_) | TaskOutcome::Blocked(_) | TaskOutcome::Killed(_)
        ),
        "{r:?}"
    );
    e.fake.clear_injections();
    e.fake.resume();
    e.gate_idle();
}
