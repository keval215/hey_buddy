//! Kill controller tests (S2-13) against a REAL `PolicyGate` (the policy crate's test rig, with
//! its manual clock and scripted desktop) and fake collaborators. No network, no devices.

#[path = "../../policy/tests/common/world.rs"]
mod world;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use buddy_core::kill::{
    FakeDriverReleaser, FakeEngineKiller, FakeRunnerHandle, FakeSpeaker, KillController,
    KillTimeouts, StepOutcome,
};
use policy::{
    ApprovedAction, AuditError, AuditRecord, AuditSink, Decision, DryRunReport, ExecResult,
    GateError, KillReason, MemoryAudit, Mode, PlanGrant, PolicyConfig, ProposedPlan, Refusal,
};
use world::*;

/// Alias so no `ApprovedAction` is followed by `{` (the CI tripwire in ci.yml).
type Act = ApprovedAction;

const WAIT: Duration = Duration::from_secs(20);

fn plan5() -> String {
    steps_json(&[
        OBSERVE,
        &click("Notes"),
        &type_into("Name", "John Smith"),
        &click("Refresh"),
        &click("Save"),
    ])
}

struct Fakes {
    ctl: Arc<KillController>,
    engine: Arc<FakeEngineKiller>,
    driver: Arc<FakeDriverReleaser>,
    speaker: Arc<FakeSpeaker>,
    runner: Arc<FakeRunnerHandle>,
}

fn controller(rig: &Rig, timeouts: KillTimeouts) -> Fakes {
    let ctl = KillController::with_timeouts(rig.gate.clone(), timeouts);
    let f = Fakes {
        ctl,
        engine: Arc::new(FakeEngineKiller::default()),
        driver: Arc::new(FakeDriverReleaser::default()),
        speaker: Arc::new(FakeSpeaker::default()),
        runner: Arc::new(FakeRunnerHandle::default()),
    };
    f.ctl.set_engine_killer(f.engine.clone());
    f.ctl.set_driver_releaser(f.driver.clone());
    f.ctl.set_speaker(f.speaker.clone());
    f.ctl.set_runner(f.runner.clone());
    f
}

fn quick() -> KillTimeouts {
    KillTimeouts {
        step: Duration::from_millis(400),
        speak: Duration::from_millis(400),
    }
}

fn calls(a: &AtomicU64) -> u64 {
    a.load(Ordering::SeqCst)
}

fn live(rig: &Rig) -> (ProposedPlan, PlanGrant) {
    rig.go_live(&plan5())
}

fn action_at(rig: &Rig, plan: &ProposedPlan, grant: &mut PlanGrant, i: usize) -> Act {
    rig.advance(300);
    let s = rig.bind(plan, i);
    rig.gate.authorize(s, grant, None).expect("mint")
}

/// Runs the read step 0, then mints step 1 (the first action).
fn first_action(rig: &Rig, plan: &ProposedPlan, grant: &mut PlanGrant) -> Act {
    rig.run_step(plan, grant, 0);
    action_at(rig, plan, grant, 1)
}

/// A fake driver: executes an action in chunks, calling `check_live` before each one.
#[derive(Default)]
struct FakeDriver {
    chunks_done: AtomicU64,
    check_failures: AtomicU64,
}

impl FakeDriver {
    fn execute(&self, a: &ApprovedAction, chunks: u64, between: &dyn Fn(u64)) -> bool {
        for i in 0..chunks {
            if a.check_live().is_err() {
                self.check_failures.fetch_add(1, Ordering::SeqCst);
                return false;
            }
            self.chunks_done.fetch_add(1, Ordering::SeqCst);
            between(i);
        }
        true
    }
}

#[test]
fn kill_during_a_running_plan_stops_within_one_step() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    let drv = FakeDriver::default();
    rig.run_step(&plan, &mut grant, 0);
    // Step 1 is mid-execution (chunk 1 of 4) when the kill arrives from another thread.
    let a = action_at(&rig, &plan, &mut grant, 1);
    let ctl = f.ctl.clone();
    let finished = drv.execute(&a, 4, &|i| {
        if i == 1 {
            let c = ctl.clone();
            thread::spawn(move || {
                c.trip(KillReason::Hotkey);
            })
            .join()
            .unwrap();
        }
    });
    assert!(!finished, "the driver stopped part way");
    assert_eq!(drv.chunks_done.load(Ordering::SeqCst), 2);
    assert_eq!(drv.check_failures.load(Ordering::SeqCst), 1);
    assert!(f.ctl.wait_idle(WAIT));
    rig.gate
        .record_result(a.digest(), &ExecResult::failed("revoked"));
    // The runner's next step cannot mint: nothing runs after the kill.
    rig.advance(300);
    let s = rig.bind(&plan, 2);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(calls(&f.engine.calls), 1);
    assert_eq!(calls(&f.runner.calls), 1);
    assert_eq!(calls(&f.driver.calls), 2);
    assert_eq!(calls(&f.speaker.calls), 1);
    let rep = f.ctl.last_report().unwrap();
    assert!(rep.observe);
    assert_eq!(rep.gate, StepOutcome::Done);
    assert_eq!(rep.reason, KillReason::Hotkey);
}

#[test]
fn after_a_kill_nothing_resumes_without_a_new_plan_and_new_approvals() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    rig.run_step(&plan, &mut grant, 0);
    let a = action_at(&rig, &plan, &mut grant, 1);
    let report = rig.dry(&plan);
    assert!(a.check_live().is_ok());
    f.ctl.trip(KillReason::Tray);
    // The epoch moved before trip returned: the action is dead at once.
    assert!(a.check_live().is_err());
    assert!(f.ctl.wait_idle(WAIT));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(rig.gate.current_task(), None);
    assert!(a.check_live().is_err());
    // The old grant mints nothing, the retired plan cannot be approved again.
    rig.advance(300);
    let s = rig.bind(&plan, 2);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
    assert!(rig.gate.request_plan_approval(&plan, &report).is_err());
    // A new propose + new approval works.
    let (plan2, mut grant2) = live(&rig);
    let a2 = first_action(&rig, &plan2, &mut grant2);
    assert!(a2.check_live().is_ok());
    assert!(a.check_live().is_err(), "the old one stays dead");
    // Audit shows `killed`, the events show `Killed`.
    assert!(rig.audit.kills().iter().any(|r| r.event == "killed"));
    assert!(rig
        .events
        .kinds()
        .iter()
        .any(|k| matches!(k, policy::AgentEventKind::Killed { reason: "tray" })));
}

#[test]
fn pending_approvals_void_after_a_kill() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let plan = rig.propose(&plan5());
    let report: DryRunReport = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let id = pending.id();
    let digest = pending.digest_hex();
    let waiter = thread::spawn(move || pending.wait().err());
    thread::sleep(Duration::from_millis(50));
    f.ctl.trip(KillReason::StopButton);
    assert_eq!(waiter.join().unwrap(), Some(GateError::Voided));
    assert!(f.ctl.wait_idle(WAIT));
    rig.advance(600);
    // Deciding on the voided approval does nothing.
    assert!(rig.gate.decide(id, &digest, Decision::Approve).is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
}

#[test]
fn a_hung_engine_killer_does_not_stop_the_other_steps() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    f.engine.hang.store(true, Ordering::SeqCst);
    let (plan, mut grant) = live(&rig);
    let a = first_action(&rig, &plan, &mut grant);
    let t0 = Instant::now();
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    assert!(t0.elapsed() < Duration::from_secs(10), "bounded");
    let rep = f.ctl.last_report().unwrap();
    assert_eq!(rep.engine, StepOutcome::TimedOut);
    assert_eq!(rep.runner, StepOutcome::Done);
    assert_eq!(rep.gate, StepOutcome::Done);
    assert_eq!(rep.driver, StepOutcome::Done);
    assert_eq!(rep.speaker, StepOutcome::Done);
    assert!(rep.observe);
    assert_eq!(calls(&f.driver.calls), 2);
    assert_eq!(calls(&f.speaker.calls), 1);
    assert!(a.check_live().is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
}

#[test]
fn a_panicking_releaser_a_blocked_speaker_and_a_dead_engine_are_isolated() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    f.driver.boom.store(true, Ordering::SeqCst);
    f.speaker.hang.store(true, Ordering::SeqCst);
    f.engine.boom.store(true, Ordering::SeqCst);
    let (_plan, _grant) = live(&rig);
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    let rep = f.ctl.last_report().unwrap();
    assert_eq!(rep.driver, StepOutcome::Panicked);
    assert_eq!(rep.engine, StepOutcome::Panicked);
    assert_eq!(rep.speaker, StepOutcome::TimedOut);
    assert_eq!(rep.gate, StepOutcome::Done);
    assert_eq!(rep.runner, StepOutcome::Done);
    assert!(rep.observe);
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(calls(&f.runner.calls), 1);
    // The controller still works afterwards.
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
}

/// An audit sink whose `killed` panics.
struct BadKilledAudit(Arc<MemoryAudit>);
impl AuditSink for BadKilledAudit {
    fn intent(&self, r: &AuditRecord) -> Result<(), AuditError> {
        self.0.intent(r)
    }
    fn record(&self, r: &AuditRecord) {
        self.0.record(r);
    }
    fn killed(&self, _r: &AuditRecord) {
        panic!("audit exploded");
    }
}

#[test]
fn an_audit_failure_does_not_stop_the_rest() {
    let rig = Rig::with_sink(PolicyConfig::default(), |m| Arc::new(BadKilledAudit(m)));
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    let a = first_action(&rig, &plan, &mut grant);
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    assert_eq!(calls(&f.driver.calls), 2);
    assert_eq!(calls(&f.engine.calls), 1);
    assert_eq!(calls(&f.speaker.calls), 1);
    assert!(a.check_live().is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(rig.gate.current_task(), None);
    // The gate still works for a new plan.
    let (plan2, mut g2) = live(&rig);
    assert!(first_action(&rig, &plan2, &mut g2).check_live().is_ok());
}

#[test]
fn a_kill_with_no_agent_running_is_harmless_and_double_trips_are_idempotent() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let e1 = f.ctl.trip(KillReason::Hotkey);
    let e2 = f.ctl.trip(KillReason::Tray);
    assert!(e2 > e1);
    assert!(f.ctl.wait_idle(WAIT));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(f.ctl.last_report().unwrap().observe);
    // A task started afterwards works normally.
    let (plan, mut grant) = live(&rig);
    assert!(first_action(&rig, &plan, &mut grant).check_live().is_ok());
}

#[test]
fn held_keys_are_released_on_every_trip() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    assert_eq!(calls(&f.driver.calls), 2, "released twice per trip");
    f.ctl.trip(KillReason::Limit);
    assert!(f.ctl.wait_idle(WAIT));
    assert!(calls(&f.driver.calls) >= 4);
    f.ctl.trip_limit();
    assert!(f.ctl.wait_idle(WAIT));
    assert_eq!(f.ctl.last_report().unwrap().reason, KillReason::Limit);
}

/// Blocks the n-th `intent` until released.
struct BlockingAudit {
    mem: Arc<MemoryAudit>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}
impl AuditSink for BlockingAudit {
    fn intent(&self, r: &AuditRecord) -> Result<(), AuditError> {
        let e = self.entered.lock().unwrap().take();
        let rel = self.release.lock().unwrap().take();
        if let (Some(e), Some(rel)) = (e, rel) {
            e.send(()).ok();
            rel.recv().ok();
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

#[test]
fn a_kill_while_reserving_refuses_the_late_mint() {
    let (etx, erx) = mpsc::channel();
    let (rtx, rrx) = mpsc::channel();
    let rig = Rig::with_sink(PolicyConfig::default(), move |m| {
        Arc::new(BlockingAudit {
            mem: m,
            entered: Mutex::new(Some(etx)),
            release: Mutex::new(Some(rrx)),
        })
    });
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    rig.run_step(&plan, &mut grant, 0);
    rig.advance(300);
    let s = rig.bind(&plan, 1);
    let gate = rig.gate.clone();
    let h = thread::spawn(move || gate.authorize(s, &mut grant, None).map(|a| a.digest()));
    erx.recv().unwrap();
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    rtx.send(()).unwrap();
    let r = h.join().unwrap();
    assert!(matches!(r, Err(Refusal::Killed | Refusal::Voided)), "{r:?}");
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(rig.gate.activity().busy_state, policy::BusyState::Idle);
}

#[test]
fn a_kill_while_an_action_is_outstanding_revokes_it() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    let a = first_action(&rig, &plan, &mut grant);
    assert_eq!(
        rig.gate.activity().busy_state,
        policy::BusyState::Outstanding
    );
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    assert!(a.check_live().is_err());
    assert!(rig.gate.activity().is_idle());
    // The driver's late report is recorded content-free and changes nothing.
    rig.gate
        .record_result(a.digest(), &ExecResult::failed("revoked"));
    assert!(rig.audit.records().iter().any(|r| r.event == "late_result"));
}

#[test]
fn a_kill_racing_authorize_never_leaves_a_live_action() {
    for round in 0..40u32 {
        let rig = Rig::new();
        let f = controller(&rig, quick());
        let (plan, mut grant) = live(&rig);
        rig.run_step(&plan, &mut grant, 0);
        rig.advance(300);
        let s = rig.bind(&plan, 1);
        let gate = rig.gate.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let b2 = barrier.clone();
        let h = thread::spawn(move || {
            b2.wait();
            gate.authorize(s, &mut grant, None)
        });
        let c = f.ctl.clone();
        let t = thread::spawn(move || {
            barrier.wait();
            if round % 2 == 0 {
                thread::yield_now();
            }
            c.trip(KillReason::Hotkey)
        });
        t.join().unwrap();
        let res = h.join().unwrap();
        // The trip has returned: the epoch moved, so whatever was minted is dead now.
        if let Ok(a) = res {
            assert!(a.check_live().is_err(), "round {round}");
        }
        assert!(f.ctl.wait_idle(WAIT));
        assert_eq!(rig.gate.mode(), Mode::Observe);
    }
}

#[test]
fn a_hundred_random_trips_from_many_threads_end_clean() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let (plan, mut grant) = live(&rig);
    let a = first_action(&rig, &plan, &mut grant);
    let ctl = f.ctl.clone();
    let total = Arc::new(AtomicU64::new(0));
    let handles: Vec<_> = (0..10u64)
        .map(|n| {
            let c = ctl.clone();
            let total = total.clone();
            thread::spawn(move || {
                let mut x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                for _ in 0..10 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    thread::sleep(Duration::from_micros(x % 500));
                    let reason = match x % 4 {
                        0 => KillReason::Hotkey,
                        1 => KillReason::Tray,
                        2 => KillReason::StopButton,
                        _ => KillReason::Limit,
                    };
                    c.trip(reason);
                    total.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(total.load(Ordering::SeqCst), 100);
    assert!(f.ctl.wait_idle(WAIT));
    assert!(a.check_live().is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.activity().is_idle());
    assert!(calls(&f.driver.calls) >= 1);
    // Still usable.
    let (plan2, mut g2) = live(&rig);
    assert!(first_action(&rig, &plan2, &mut g2).check_live().is_ok());
}

/// A runner whose abort is observed by the driver releaser: proves the order.
struct OrderedRunner(Arc<Mutex<Vec<&'static str>>>);
impl buddy_core::kill::RunnerHandle for OrderedRunner {
    fn abort(&self) {
        self.0.lock().unwrap().push("runner");
    }
}
struct OrderedDriver(Arc<Mutex<Vec<&'static str>>>);
impl buddy_core::kill::DriverReleaser for OrderedDriver {
    fn emergency_release(&self) {
        self.0.lock().unwrap().push("release");
    }
}

#[test]
fn the_driver_is_released_a_second_time_after_the_runner_abort() {
    let rig = Rig::new();
    let f = controller(&rig, quick());
    let log = Arc::new(Mutex::new(Vec::new()));
    f.ctl.set_runner(Arc::new(OrderedRunner(log.clone())));
    f.ctl
        .set_driver_releaser(Arc::new(OrderedDriver(log.clone())));
    f.ctl.trip(KillReason::Hotkey);
    assert!(f.ctl.wait_idle(WAIT));
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|e| **e == "release").count(),
        2,
        "{log:?}"
    );
    assert_eq!(log.last(), Some(&"release"));
    assert!(log.contains(&"runner"));
    let abort_at = log.iter().position(|e| *e == "runner").unwrap();
    let last_release = log.iter().rposition(|e| *e == "release").unwrap();
    assert!(
        last_release > abort_at,
        "second release after the runner abort: {log:?}"
    );
}

#[test]
fn a_parked_speaker_never_delays_a_second_trip() {
    let rig = Rig::new();
    let slow = KillTimeouts {
        step: Duration::from_millis(400),
        speak: Duration::from_secs(8),
    };
    let f = controller(&rig, slow);
    f.speaker.hang.store(true, Ordering::SeqCst);
    f.ctl.trip(KillReason::Hotkey);
    let t0 = Instant::now();
    while calls(&f.speaker.calls) < 1 {
        assert!(t0.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(5));
    }
    // The speaker of trip one is parked. A second trip must still run its engine kill, runner
    // abort and driver releases without waiting for the speaker's 8 s limit.
    let t1 = Instant::now();
    f.ctl.trip(KillReason::Tray);
    while calls(&f.engine.calls) < 2 || calls(&f.driver.calls) < 4 || calls(&f.runner.calls) < 2 {
        assert!(
            t1.elapsed() < Duration::from_secs(4),
            "second trip was held up"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(t1.elapsed() < Duration::from_secs(4));
    assert_eq!(rig.gate.mode(), Mode::Observe);
}
