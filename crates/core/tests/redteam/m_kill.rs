//! Case m: kill-switch storms and concurrent task spam.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::rig::*;
use agent::fakes::Scripted;
use buddy_core::agent::{RefuseReason, Stage, TaskOutcome, TaskRequest};
use buddy_core::kill::{KillController, KillReason};
use driver::fake::CallKind;

/// Trips the kill switch `n` times from another thread, `gap` apart.
fn storm(ctl: &Arc<KillController>, n: usize, gap: Duration) -> thread::JoinHandle<()> {
    let ctl = ctl.clone();
    thread::spawn(move || {
        for _ in 0..n {
            ctl.trip(KillReason::Hotkey);
            thread::sleep(gap);
        }
    })
}

/// After a kill the next task must be possible (the kill does not poison the supervisor) and
/// nothing may still be running.
async fn recovers(e: &Env) {
    assert!(
        e.ctl.wait_idle(Duration::from_secs(10)),
        "the kill sequence never finished"
    );
    e.gate_idle();
    e.fake.clear_calls();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "after the storm: {r:?}");
}

/// After the task returned nothing may still execute (no late call from a worker).
fn stable(e: &Env) {
    let n = e.fake.call_count();
    thread::sleep(Duration::from_millis(60));
    assert_eq!(
        e.fake.call_count(),
        n,
        "an action ran after the task had ended"
    );
}

/// Attack: kill storm during PLANNING (the engine is mid-plan). Expected: `Killed(planning)`,
/// nothing else ran. Defence: kill epoch checked before / after `engine.plan`; engine killed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_storm_during_planning_ends_the_task_and_runs_nothing() {
    let e = env();
    e.factory.push(Scripted::hang());
    let sup = e.sup.clone();
    let t =
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    let f = e.factory.clone();
    wait_until(|| f.plans() == 1);
    storm(&e.ctl, 20, Duration::from_millis(1)).join().unwrap();
    let r = bounded(t).await.unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    e.executed_nothing();
    recovers(&e).await;
}

/// Attack: kill storm while the screen is being observed (slow source). Expected: `Killed`.
/// Defence: epoch check after every observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_storm_during_the_observation_ends_the_task() {
    let e = env();
    e.rig.desk.slow_source(Duration::from_millis(150));
    let sup = e.sup.clone();
    e.factory.draft(&notepad_plan());
    let t =
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    thread::sleep(Duration::from_millis(60));
    storm(&e.ctl, 10, Duration::from_millis(1)).join().unwrap();
    let r = bounded(t).await.unwrap();
    assert!(matches!(r.outcome, TaskOutcome::Killed(_)), "{r:?}");
    e.executed_nothing();
    e.rig.desk.slow_source(Duration::ZERO);
    recovers(&e).await;
}

/// Attack: kill storm while the plan approval is open. Expected: `Killed(live_offer)`, the pending
/// request resolved, nothing ran. Defence: the broker resolves pending requests `Killed`.
#[tokio::test]
async fn a_kill_storm_during_the_plan_approval_resolves_it_killed() {
    let e = env();
    e.human.script(&[Act::Silent]);
    let (b, c) = (e.broker.clone(), e.ctl.clone());
    let t = thread::spawn(move || {
        wait_until(|| b.pending_count() > 0);
        storm(&c, 20, Duration::from_millis(1)).join().unwrap();
    });
    let r = e.run(&notepad_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::LiveOffer), "{r:?}");
    assert_eq!(e.broker.pending_count(), 0);
    e.executed_nothing();
    recovers(&e).await;
}

/// Attack: kill storm while the SUBMIT approval is open. Expected: `Killed(step_approval)`, no click.
#[tokio::test]
async fn a_kill_storm_during_the_submit_approval_clicks_nothing() {
    let e = env();
    e.human.script(&[Act::Approve, Act::Silent]);
    let (b, c, h) = (e.broker.clone(), e.ctl.clone(), e.human.clone());
    let t = thread::spawn(move || {
        wait_until(|| h.seen().len() == 2 && b.pending_count() > 0);
        storm(&c, 20, Duration::from_millis(1)).join().unwrap();
    });
    let r = e.run(&form_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::StepApproval), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    stable(&e);
    recovers(&e).await;
}

/// Attack: the kill lands right after EACH actuator call of a long plan (open, focus, every typed
/// chunk, between steps), and the storm continues. Expected: no further actuator call after the
/// call during which the trip landed; `Killed(run)`. Defence: kill epoch checked before and after
/// every phase, in the driver before every chunk.
#[tokio::test]
async fn a_kill_after_every_actuator_call_stops_all_later_calls() {
    let long = "x".repeat(40);
    let transcript = format!("open Notepad then type {long} into the Name field");
    let plan = steps_json(&[OPEN_NOTEPAD, &type_into("Name", &long), SCROLL1]);
    // First measure the whole call sequence.
    let e = env();
    e.factory.draft(&plan);
    let r = bounded(e.sup.run_task(TaskRequest::interactive(&transcript))).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    let total = e.fake.call_count();
    assert!(total >= 5, "control: {total} calls in a full run");

    for n in 1..total {
        let e = env();
        let ctl = e.ctl.clone();
        let fake = e.fake.clone();
        let tripped = Arc::new(AtomicBool::new(false));
        let t2 = tripped.clone();
        e.fake.set_hook(move |c| {
            if c.seq + 1 == n && !t2.swap(true, Ordering::SeqCst) {
                ctl.trip(KillReason::Hotkey);
                let _ = &fake;
            }
        });
        e.factory.draft(&plan);
        let r = bounded(e.sup.run_task(TaskRequest::interactive(&transcript))).await;
        assert!(
            matches!(
                r.outcome,
                TaskOutcome::Killed(_) | TaskOutcome::Blocked(_) | TaskOutcome::Failed(_)
            ),
            "kill after call {n}: {r:?}"
        );
        assert_eq!(
            e.fake.call_count(),
            n,
            "kill after call {n}: an action ran after the kill"
        );
        stable(&e);
        assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    }
}

/// Attack: a storm from another thread at 40 different moments of a 6-step plan.
/// Expected: in every run at most ONE actuator call finishes after the first trip landed (the one
/// already in flight), the task ends `Completed` or stopped, and nothing runs after it ended.
/// Defence: kill epoch (`is_tripped_since`) before every phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_storm_at_forty_different_moments_never_lets_a_second_call_through() {
    let plan = steps_json(&[
        SCROLL1, SCROLL_UP1, SCROLL1, SCROLL_UP1, SCROLL1, SCROLL_UP1,
    ]);
    for i in 0..40u64 {
        let e = env();
        let e0 = e.rig.kill.epoch();
        let log: Arc<Mutex<Vec<u64>>> = Arc::default();
        let (l2, kill) = (log.clone(), e.rig.kill.clone());
        e.fake
            .set_hook(move |_| l2.lock().unwrap().push(kill.epoch()));
        let storm = {
            let ctl = e.ctl.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_micros(i * 400));
                for _ in 0..10 {
                    ctl.trip(KillReason::Hotkey);
                    thread::sleep(Duration::from_micros(300));
                }
            })
        };
        let r = e.run(&plan).await;
        storm.join().unwrap();
        assert!(
            matches!(
                r.outcome,
                TaskOutcome::Completed
                    | TaskOutcome::Killed(_)
                    | TaskOutcome::Blocked(_)
                    | TaskOutcome::Failed(_)
                    | TaskOutcome::Refused(RefuseReason::Killed)
            ),
            "{i}: {r:?}"
        );
        let after = log.lock().unwrap().iter().filter(|ep| **ep > e0).count();
        assert!(
            after <= 1,
            "{i}: {after} actuator calls finished after the kill epoch moved"
        );
        stable(&e);
        assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    }
}

/// Attack: 30 concurrent `run_task` calls on one supervisor. Expected: exactly ONE runs (one
/// engine created); the other 29 are `Refused(busy)`. Defence: the one-task-at-a-time flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_run_task_spam_runs_exactly_one_task() {
    let e = env();
    e.factory.push(Scripted::hang());
    let mut tasks = Vec::new();
    for _ in 0..30 {
        let sup = e.sup.clone();
        tasks.push(tokio::spawn(async move {
            bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await
        }));
    }
    let f = e.factory.clone();
    wait_until(|| f.plans() == 1);
    thread::sleep(Duration::from_millis(100));
    e.ctl.trip(KillReason::StopButton);
    let mut busy = 0;
    let mut ran = 0;
    for t in tasks {
        let r = bounded(t).await.unwrap();
        match r.outcome {
            TaskOutcome::Refused(RefuseReason::Busy) => busy += 1,
            TaskOutcome::Killed(Stage::Planning) => ran += 1,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!((ran, busy), (1, 29));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 1);
    e.executed_nothing();
    recovers(&e).await;
}
