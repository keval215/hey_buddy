//! Supervisor tests (S2-17 + S2-23), Linux, no network, no devices. REAL `PolicyGate` (manual
//! clock), REAL `PrivacyFirewall` over the `ScriptedDesktop`, REAL `Driver` over `FakeActuator`,
//! REAL `ApprovalBroker` with a scripted "human", REAL `KillController`; the engine is a
//! `FakeEngine` / `ProcessEngine` over a `TestSandbox` (`HostileEngine`). Failure table:
//! `docs/supervisor.md`.

#[path = "../../../policy/tests/common/world.rs"]
mod world;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ::agent::fakes::{FakeEngine, Hostile, HostileEngine, Scripted};
use ::agent::sandbox::TestSandbox;
use ::agent::{
    EngineError, GatewayError, LaunchSpec, ModelCall, ModelGateway, PlanDraftJson, PlanOutcome,
    ProcessEngine,
};
use async_trait::async_trait;
use driver::fake::{CallKind, FakeActuator, Inject};
use driver::{Driver, LaunchAllowlist};
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::{NodeRole, SessionProbeState};
use frame::{AppDenyPolicy, Rect};
use policy::{
    AgentEventKind, AuditRecord, Catalogue, ManualClock, PlanContext, PlanStep, TaskOrigin,
};
use world::*;

use super::*;
use crate::approval::{
    ApprovalBroker, ApprovalClosedDto, ApprovalDecisionDto, ApprovalKindDto, ApprovalRequestDto,
    ApprovalUi,
};
use crate::kill::{KillController, KillReason, KillTimeouts};

// ---------------------------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------------------------

struct NoModels;
#[async_trait]
impl ModelGateway for NoModels {
    async fn complete(&self, _c: ModelCall) -> Result<String, GatewayError> {
        Err(GatewayError::Model("none"))
    }
}

/// Hands out `FakeEngine`s, one script per task.
#[derive(Default)]
struct Factory {
    script: Mutex<VecDeque<Scripted>>,
    last: Mutex<Option<Arc<FakeEngine>>>,
    creates: AtomicU32,
    fail: Mutex<Option<EngineError>>,
}

impl Factory {
    fn push(&self, s: Scripted) {
        self.script.lock().unwrap().push_back(s);
    }
    fn draft(&self, json: &str) {
        self.push(Scripted::ok(PlanOutcome::Draft(
            PlanDraftJson::new(json.to_owned()).unwrap(),
        )));
    }
    fn engine(&self) -> Arc<FakeEngine> {
        self.last.lock().unwrap().clone().expect("an engine")
    }
}

impl EngineFactory for Factory {
    fn create(&self, _task: ::agent::TaskId) -> Result<EngineParts, EngineError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = *self.fail.lock().unwrap() {
            return Err(e);
        }
        let s = self.script.lock().unwrap().pop_front();
        let e = Arc::new(FakeEngine::new(s.into_iter().collect()));
        *self.last.lock().unwrap() = Some(e.clone());
        Ok(EngineParts {
            engine: e,
            gateway: Arc::new(NoModels),
            core_gateway: None,
        })
    }
}

/// A `ProcessEngine` over a test sandbox (hostile child) or the unsupported sandbox.
struct ProcFactory(Arc<dyn ::agent::EngineSandbox>);
impl EngineFactory for ProcFactory {
    fn create(&self, _task: ::agent::TaskId) -> Result<EngineParts, EngineError> {
        let spec = LaunchSpec {
            exe: "/opt/hey_buddy/buddy-agent".into(),
            install_dir: "/opt/hey_buddy".into(),
        };
        Ok(EngineParts {
            engine: Arc::new(ProcessEngine::new(self.0.clone(), spec, 1)),
            gateway: Arc::new(NoModels),
            core_gateway: None,
        })
    }
}

struct ManualPace(Vec<Arc<ManualClock>>);
#[async_trait]
impl Pace for ManualPace {
    async fn pause(&self, d: Duration) {
        for c in &self.0 {
            c.advance(d);
        }
    }
}

struct Deny(&'static [&'static str]);
impl AppDenyPolicy for Deny {
    fn is_denied(&self, a: &str) -> bool {
        self.0.iter().any(|d| d.eq_ignore_ascii_case(a))
    }
}

#[derive(Clone, Copy)]
enum Act {
    Approve,
    Deny,
    /// Shown, never answered (the test answers by clock, kill or nothing).
    Silent,
}

/// The "user": answers each request as shown, in order (then approves).
struct Human {
    broker: Mutex<Option<ApprovalBroker>>,
    clock: Arc<ManualClock>,
    script: Mutex<VecDeque<Act>>,
    seen: Mutex<Vec<ApprovalRequestDto>>,
}

impl Human {
    fn script(&self, acts: &[Act]) {
        *self.script.lock().unwrap() = acts.iter().copied().collect();
    }
    fn seen(&self) -> Vec<ApprovalRequestDto> {
        self.seen.lock().unwrap().clone()
    }
}

impl ApprovalUi for Human {
    fn show(&self, r: &ApprovalRequestDto) -> bool {
        self.seen.lock().unwrap().push(r.clone());
        let act = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Act::Approve);
        let broker = self.broker.lock().unwrap().clone().expect("broker");
        match act {
            Act::Approve => {
                broker.mark_displayed(&r.id).unwrap();
                self.clock.advance(Duration::from_millis(600));
                broker
                    .respond(&r.id, ApprovalDecisionDto::Approve, &r.digest_hex)
                    .unwrap();
            }
            Act::Deny => {
                broker
                    .respond(&r.id, ApprovalDecisionDto::Deny, &r.digest_hex)
                    .unwrap();
            }
            Act::Silent => {}
        }
        true
    }
    fn close(&self, _c: &ApprovalClosedDto) {}
}

struct Opts {
    settings: FixedSettings,
    desk: ScriptedDesktop,
    deny: Arc<dyn AppDenyPolicy>,
    limits: Option<RunLimits>,
    unattended: Option<RunLimits>,
    facts: Arc<dyn BindFacts>,
    factory: Option<Arc<dyn EngineFactory>>,
    audit: Option<Arc<dyn policy::AuditSink>>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            settings: FixedSettings::default(),
            desk: desktop(),
            deny: Arc::new(Deny(&[])),
            limits: None,
            unattended: None,
            facts: Arc::new(ReverifiedAtExecute),
            factory: None,
            audit: None,
        }
    }
}

struct Env {
    rig: Rig,
    sup: Arc<AgentSupervisor>,
    ctl: Arc<KillController>,
    broker: ApprovalBroker,
    fake: FakeActuator,
    human: Arc<Human>,
    factory: Arc<Factory>,
    notes: Arc<RecordingNotifier>,
}

fn quick() -> KillTimeouts {
    KillTimeouts {
        step: Duration::from_millis(400),
        speak: Duration::from_millis(400),
    }
}

fn env() -> Env {
    env_with(Opts::default())
}

fn env_with(o: Opts) -> Env {
    let mut rig = match &o.audit {
        Some(a) => {
            let a = a.clone();
            Rig::with_sink(policy::PolicyConfig::default(), move |_m| a)
        }
        None => Rig::new(),
    };
    rig.desk = o.desk.clone();
    let ctl = KillController::with_timeouts(rig.gate.clone(), quick());
    let fw = Arc::new(firewall_of(&rig.desk));
    let fake = FakeActuator::new(rig.desk.clone());
    let dclock = Arc::new(ManualClock::new());
    dclock.advance(Duration::from_secs(1000));
    let launch = LaunchAllowlist::empty()
        .with("notepad", "C:\\Windows\\System32\\notepad.exe")
        .unwrap();
    let drv = Arc::new(
        Driver::new(fw, fake.clone())
            .with_clock(dclock.clone())
            .with_launch_allowlist(launch)
            .with_call_deadline(Duration::from_millis(800)),
    );
    let broker = ApprovalBroker::new(rig.clock.clone(), &rig.kill);
    let human = Arc::new(Human {
        broker: Mutex::new(Some(broker.clone())),
        clock: rig.clock.clone(),
        script: Mutex::new(VecDeque::new()),
        seen: Mutex::new(Vec::new()),
    });
    broker.set_ui(human.clone());
    let factory = Arc::new(Factory::default());
    let notes = Arc::new(RecordingNotifier::new());
    let audit: Arc<dyn policy::AuditSink> = o.audit.clone().unwrap_or_else(|| rig.audit.clone());
    let efactory: Arc<dyn EngineFactory> = match &o.factory {
        Some(f) => f.clone(),
        None => factory.clone(),
    };
    let mut parts = SupervisorParts::new(
        ctl.clone(),
        audit,
        broker.clone(),
        drv.clone(),
        efactory,
        Arc::new(o.settings),
        Arc::new(FixedDeny(o.deny.clone())),
        Arc::new(catalogue()),
    );
    parts.facts = o.facts.clone();
    parts.notifier = notes.clone();
    parts.pace = Arc::new(ManualPace(vec![rig.clock.clone(), dclock.clone()]));
    if let Some(l) = o.limits {
        parts.interactive_limits = l;
    }
    if let Some(l) = o.unattended {
        parts.unattended_limits = l;
    }
    let sup = Arc::new(AgentSupervisor::new(parts));
    sup.wire_kill();
    Env {
        rig,
        sup,
        ctl,
        broker,
        fake,
        human,
        factory,
        notes,
    }
}

const SCROLL1: &str = r#"{"op":"scroll","direction":"down","amount":1}"#;
const SCROLL_UP1: &str = r#"{"op":"scroll","direction":"up","amount":1}"#;

fn notepad_plan() -> String {
    steps_json(&[OPEN_NOTEPAD, &type_into("Name", "John Smith")])
}

impl Env {
    async fn run_as(&self, origin: RequestOrigin, plan: &str) -> TaskReport {
        self.factory.draft(plan);
        self.sup
            .run_task(TaskRequest::new(origin, TRANSCRIPT))
            .await
    }
    async fn run(&self, plan: &str) -> TaskReport {
        self.run_as(RequestOrigin::Interactive, plan).await
    }
    fn executed_nothing(&self) {
        assert_eq!(
            self.fake.call_count(),
            0,
            "the actuator must not have been called"
        );
        assert!(self.rig.audit.intents().is_empty(), "no intent was written");
    }
    fn gate_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.rig.gate.activity().is_idle() {
            assert!(Instant::now() < deadline, "the gate is still busy");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn trip_when_pending(&self) -> thread::JoinHandle<()> {
        let (b, c) = (self.broker.clone(), self.ctl.clone());
        thread::spawn(move || {
            wait_until(|| b.pending_count() > 0);
            c.trip(KillReason::Hotkey);
        })
    }
    fn advance_when_pending(&self, by: Duration) -> thread::JoinHandle<()> {
        let (b, clock) = (self.broker.clone(), self.rig.clock.clone());
        thread::spawn(move || {
            wait_until(|| b.pending_count() > 0);
            clock.advance(by);
        })
    }
    fn events(&self) -> Vec<&'static str> {
        self.rig.audit.records().iter().map(|r| r.event).collect()
    }
}

fn wait_until(mut c: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !c() {
        assert!(Instant::now() < deadline, "timed out waiting");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Every string field of every record, joined (for leak scans).
fn audit_text(records: &[AuditRecord]) -> String {
    let mut s = String::new();
    for r in records {
        let parts = [
            Some(r.event.to_owned()),
            r.plan_digest8.clone(),
            r.action_digest8.clone(),
            r.op.map(str::to_owned),
            r.target_role.map(str::to_owned),
            r.target_label.clone(),
            r.app_id.clone(),
            r.tier.map(str::to_owned),
            r.decision.map(str::to_owned),
            r.result_code.map(str::to_owned),
        ];
        for p in parts.into_iter().flatten() {
            s.push_str(&p);
            s.push('\n');
        }
    }
    s
}

fn assert_send<T: Send>(_: &T) {}

// ---------------------------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn read_only_task_runs_end_to_end_on_fakes() {
    let e = env();
    let plan = steps_json(&[OBSERVE, r#"{"op":"find_text","query":"Save"}"#]);
    e.factory.draft(&plan);
    let fut = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT));
    assert_send(&fut);
    let r = fut.await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert!(r.dry_run && r.live);
    assert_eq!((r.would_do, r.steps_done, r.executed), (2, 2, 0));
    // Reads never reach the driver; the broker was asked exactly once ("Run it for real?").
    assert_eq!(e.fake.call_count(), 0);
    let seen = e.human.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].summary.starts_with("Run it for real? Plan "));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 1);
    assert_eq!(
        e.factory.engine().plans.load(Ordering::SeqCst),
        1,
        "one plan call"
    );
    e.gate_idle();
}

#[tokio::test]
async fn notepad_dry_run_executes_nothing_then_live_executes_exactly_the_plan() {
    let e = env();
    // 1. Dry run, the user declines going live.
    e.human.script(&[Act::Deny]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer), "{r:?}");
    assert!(r.dry_run && !r.live);
    assert_eq!(r.would_do, 2);
    e.executed_nothing();
    assert!(e.events().contains(&"sup_would_do"));
    e.gate_idle();

    // 2. Same request, user says "run it for real": exactly the planned steps run.
    e.human.script(&[Act::Approve]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert!(r.live);
    assert_eq!((r.steps_done, r.executed), (2, 2));
    assert_eq!(e.fake.count(CallKind::Launch), 1);
    assert_eq!(e.fake.chars_sent(), "John Smith".len());
    // open_app: 1 launch; type: set_focus + chunks. Nothing else (no click, no key).
    for k in [
        CallKind::Invoke,
        CallKind::SendKey,
        CallKind::SendCopy,
        CallKind::Scroll,
    ] {
        assert_eq!(e.fake.count(k), 0);
    }
    // Write-ahead intent for every executed step, from the gate.
    assert_eq!(e.rig.audit.intents().len(), 2);
    // Live is for THAT task only: the gate is back in Observe mode and idle.
    assert_eq!(e.rig.gate.mode(), policy::Mode::Observe);
    e.gate_idle();
    // The two questions were asked as "Run it for real?" and nothing else.
    let seen = e.human.seen();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|s| s.kind == ApprovalKindDto::Plan));
    assert!(seen
        .iter()
        .all(|s| s.summary.starts_with("Run it for real? ")));
}

#[tokio::test]
async fn observe_mode_off_asks_a_plain_plan_approval_and_skips_nothing_else() {
    let e = env_with(Opts {
        settings: FixedSettings {
            observe_mode: false,
            ..FixedSettings::default()
        },
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    let seen = e.human.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].summary.starts_with("Plan "));
    assert!(!seen[0].summary.starts_with("Run it for real?"));
}

#[tokio::test]
async fn the_planner_gets_the_transcript_and_context_only() {
    // The engine is called once with the spoken words; the foreground app is passed only when
    // the screen was Clear and the app is not deny-listed.
    let e = env();
    e.factory.draft(&steps_json(&[OBSERVE]));
    let _ = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
    assert_eq!(e.factory.engine().plans.load(Ordering::SeqCst), 1);
    // A deny-listed foreground app: observation is withheld, nothing is passed and the task is
    // refused (no observation, no dry run).
    let e = env_with(Opts {
        deny: Arc::new(Deny(&["notes.exe"])),
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::ObservationUnavailable)
    );
    e.executed_nothing();
    e.gate_idle();
}

// ---------------------------------------------------------------------------------------------
// Fail closed before anything
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn refused_when_disabled_without_probe_or_with_a_kill_in_flight() {
    let e = env_with(Opts {
        settings: FixedSettings {
            agent_enabled: false,
            ..FixedSettings::default()
        },
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::AgentDisabled));
    assert_eq!(r.task, 0);
    assert!(e.events().contains(&"sup_refused"));
    assert_eq!(
        e.factory.creates.load(Ordering::SeqCst),
        0,
        "no engine made"
    );
    e.executed_nothing();

    let e = env_with(Opts {
        settings: FixedSettings {
            probe: false,
            ..FixedSettings::default()
        },
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::NoActivityProbe)
    );
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 0);

    // A kill that is still being carried out (a hung engine killer) refuses a new task.
    let e = env();
    let hang = Arc::new(crate::kill::FakeEngineKiller::default());
    hang.hang.store(true, Ordering::SeqCst);
    e.ctl.set_engine_killer(hang);
    e.ctl.trip(KillReason::Hotkey);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::Killed),
        "{r:?}"
    );
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    // After the kill finished a new task is allowed again (a new plan and new approvals).
    e.sup.wire_kill();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
}

#[tokio::test]
async fn sandbox_unavailable_and_engine_unavailable_refuse_the_task() {
    // No sandbox on this platform: the real ProcessEngine over UnsupportedSandbox.
    let e = env_with(Opts {
        factory: Some(Arc::new(ProcFactory(Arc::new(::agent::UnsupportedSandbox)))),
        ..Opts::default()
    });
    e.factory.draft("unused");
    let r = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::SandboxUnavailable),
        "{r:?}"
    );
    e.executed_nothing();
    e.gate_idle();

    // The factory itself fails.
    let e = env();
    *e.factory.fail.lock().unwrap() = Some(EngineError::Unavailable("spawn_failed"));
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::EngineUnavailable)
    );
    *e.factory.fail.lock().unwrap() = Some(EngineError::Unavailable("sandbox_failed"));
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::SandboxUnavailable)
    );
    e.executed_nothing();
}

#[tokio::test]
async fn empty_or_oversize_requests_are_refused() {
    let e = env();
    for t in ["", "   \n", &"x".repeat(5000)] {
        let r = e.sup.run_task(TaskRequest::interactive(t)).await;
        assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::BadRequest));
    }
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_task_while_one_runs_is_refused_busy() {
    let e = env();
    e.factory.push(Scripted::hang());
    let sup = e.sup.clone();
    let first =
        tokio::spawn(async move { sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await });
    let eng = e.factory.clone();
    wait_until(|| {
        eng.last
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|x| x.plans.load(Ordering::SeqCst) == 1)
    });
    let second = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
    assert_eq!(second.outcome, TaskOutcome::Refused(RefuseReason::Busy));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 1);
    // The running task is untouched until it is stopped.
    e.ctl.trip(KillReason::StopButton);
    let r = first.await.unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

// ---------------------------------------------------------------------------------------------
// The engine is untrusted; policy is the authority
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn policy_rejects_unknown_ops_extra_fields_and_invented_apps() {
    for (plan, code) in [
        (r#"{"steps":[{"op":"shell","cmd":"del *"}]}"#, "unknown_op"),
        (
            r#"{"steps":[{"op":"click","target":{"label":"Save"},"tier":"read"}]}"#,
            "unknown_field",
        ),
        (
            r#"{"steps":[{"op":"open_app","app":"Regedit"}]}"#,
            "app_not_in_request",
        ),
        ("not json", ""),
    ] {
        let e = env();
        let r = e.run(plan).await;
        match r.outcome {
            TaskOutcome::PlanRejected(c) => {
                if !code.is_empty() {
                    assert_eq!(c, code, "{plan}");
                }
            }
            other => panic!("expected PlanRejected, got {other:?}"),
        }
        e.executed_nothing();
        e.gate_idle();
    }
}

#[tokio::test]
async fn cannot_plan_and_engine_failures_are_typed_and_run_nothing() {
    let e = env();
    e.factory
        .push(Scripted::ok(PlanOutcome::CannotPlan("cannot_plan")));
    let r = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::CannotPlan));
    e.factory
        .push(Scripted::err(EngineError::Protocol("bad_line")));
    let r = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
    assert_eq!(r.outcome, TaskOutcome::PlanFailed("engine_protocol"));
    e.executed_nothing();
}

#[tokio::test]
async fn a_hostile_engine_gets_a_protocol_error_and_nothing_runs() {
    for what in Hostile::all() {
        let sandbox = Arc::new(TestSandbox::in_process(
            HostileEngine::new(*what).into_main(),
        ));
        let e = env_with(Opts {
            factory: Some(Arc::new(ProcFactory(sandbox))),
            ..Opts::default()
        });
        let r = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
        assert!(
            matches!(
                r.outcome,
                TaskOutcome::PlanFailed(_) | TaskOutcome::PlanRejected(_)
            ),
            "{what:?}: {r:?}"
        );
        // Forged messages and malformed lines are protocol violations.
        if matches!(
            what,
            Hostile::ForgedApprove
                | Hostile::ForgedExecute
                | Hostile::ForgedObserve
                | Hostile::ForgedHello
                | Hostile::InvalidUtf8
                | Hostile::NulByte
                | Hostile::EmptyLine
        ) {
            assert_eq!(
                r.outcome,
                TaskOutcome::PlanFailed("engine_protocol"),
                "{what:?}"
            );
        }
        e.executed_nothing();
        assert!(
            e.human.seen().is_empty(),
            "nothing was ever asked: {what:?}"
        );
        e.gate_idle();
    }
}

// ---------------------------------------------------------------------------------------------
// Approvals: deny / expire / kill at every point
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn plan_approval_denied_ends_the_task_and_audits() {
    let e = env_with(Opts {
        settings: FixedSettings {
            observe_mode: false,
            ..FixedSettings::default()
        },
        ..Opts::default()
    });
    e.human.script(&[Act::Deny]);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::PlanApproval), "{r:?}");
    e.executed_nothing();
    assert!(e.events().contains(&"sup_task_end"));
    e.gate_idle();
}

#[tokio::test]
async fn plan_approval_expired_ends_the_task() {
    let e = env();
    e.human.script(&[Act::Silent]);
    let t = e.advance_when_pending(Duration::from_secs(130));
    let r = e.run(&notepad_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Expired(Stage::LiveOffer), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

#[tokio::test]
async fn kill_while_waiting_for_the_plan_approval_resolves_killed() {
    let e = env();
    e.human.script(&[Act::Silent]);
    let t = e.trip_when_pending();
    let r = e.run(&notepad_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::LiveOffer), "{r:?}");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.executed_nothing();
    assert_eq!(e.broker.pending_count(), 0, "pending approvals resolved");
    e.gate_idle();
}

fn form_plan() -> String {
    steps_json(&[&type_into("Name", "John Smith"), &click("Save")])
}

#[tokio::test]
async fn submit_is_its_own_approval_and_plan_approval_alone_stops_before_it() {
    let e = env();
    // Plan: yes. Submit: no.
    e.human.script(&[Act::Approve, Act::Deny]);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::StepApproval), "{r:?}");
    // The typing was covered by the plan approval; the click never happened.
    assert_eq!(e.fake.chars_sent(), "John Smith".len());
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "submit never ran");
    let seen = e.human.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].kind, ApprovalKindDto::Submit);
    assert!(seen[1].summary.contains(", submit"));
    assert_ne!(seen[0].digest_hex, seen[1].digest_hex);
    e.gate_idle();

    // With its own approval the submit runs, once.
    e.fake.clear_calls();
    e.human.script(&[Act::Approve, Act::Approve]);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 1);
    // One intent from the first run (the typing) plus two from this one.
    assert_eq!(e.rig.audit.intents().len(), 3);
}

#[tokio::test]
async fn step_approval_expired_and_kill_during_step_approval() {
    let e = env();
    e.human.script(&[Act::Approve, Act::Silent]);
    let t = e.advance_when_pending(Duration::from_secs(40));
    let r = e.run(&form_plan()).await;
    t.join().unwrap();
    assert_eq!(
        r.outcome,
        TaskOutcome::Expired(Stage::StepApproval),
        "{r:?}"
    );
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    e.gate_idle();

    let e = env();
    e.human.script(&[Act::Approve, Act::Silent]);
    // The first pending request is the plan; wait for the SECOND (the step) before tripping.
    let (b, c, h) = (e.broker.clone(), e.ctl.clone(), e.human.clone());
    let t = thread::spawn(move || {
        wait_until(|| h.seen().len() == 2 && b.pending_count() > 0);
        c.trip(KillReason::Tray);
    });
    let r = e.run(&form_plan()).await;
    t.join().unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::StepApproval), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_during_planning_kills_the_engine_and_stops() {
    let e = env();
    e.factory.push(Scripted::hang());
    let sup = e.sup.clone();
    let task =
        tokio::spawn(async move { sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await });
    let f = e.factory.clone();
    wait_until(|| {
        f.last
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|x| x.plans.load(Ordering::SeqCst) == 1)
    });
    e.ctl.trip(KillReason::Hotkey);
    let r = task.await.unwrap();
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    assert!(
        e.factory.engine().kills.load(Ordering::SeqCst) >= 1,
        "engine killed"
    );
    e.executed_nothing();
}

#[tokio::test]
async fn kill_inside_the_driver_stops_typing_within_one_chunk() {
    let e = env();
    let long = "x".repeat(40);
    let transcript = format!("open Notepad then type {long} into the Name field");
    let plan = steps_json(&[&type_into("Name", &long)]);
    // The kill arrives from another thread while the first chunk is being typed.
    let ctl = e.ctl.clone();
    let tripped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let t2 = tripped.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::SendChars && !t2.swap(true, Ordering::SeqCst) {
            ctl.trip(KillReason::Hotkey);
        }
    });
    e.factory.draft(&plan);
    let r = e.sup.run_task(TaskRequest::interactive(transcript)).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(e.fake.chunk_lens(), vec![16], "stopped after one chunk");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
}

#[tokio::test]
async fn kill_between_steps_stops_before_the_next_step() {
    let e = env();
    let ctl = e.ctl.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            ctl.trip(KillReason::Hotkey);
        }
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Launch), 1);
    assert_eq!(e.fake.chars_sent(), 0, "the second step never started");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

#[tokio::test]
async fn wire_kill_reaches_the_engine_the_driver_and_the_runner() {
    let e = env();
    let eng = Arc::new(FakeEngine::new(vec![]));
    e.sup.engine_killer().attach(eng.clone());
    assert!(!e.sup.aborted());
    e.ctl.trip(KillReason::StopButton);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    assert_eq!(eng.kills.load(Ordering::SeqCst), 1, "engine killer wired");
    assert!(e.fake.releases() >= 2, "driver released twice per trip");
    assert!(e.sup.aborted(), "runner abort wired");
    let rep = e.ctl.last_report().unwrap();
    assert!(rep.observe);
}

#[tokio::test]
async fn driver_failure_ends_the_task_with_a_token() {
    let e = env();
    e.fake.inject(
        CallKind::SendChars,
        Inject::Fail(driver::ActError::Platform("boom")),
    );
    let r = e.run(&notepad_plan()).await;
    assert!(matches!(r.outcome, TaskOutcome::Failed(_)), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Launch), 1);
    e.gate_idle();
}

// ---------------------------------------------------------------------------------------------
// Digest, frozen plan, replay, observation reuse
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_digest_mismatch_between_broker_and_gate_aborts() {
    let e = env();
    e.sup.hooks.digest_tamper.store(true, Ordering::SeqCst);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("digest_mismatch"), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

#[tokio::test]
async fn a_step_outside_the_frozen_plan_is_blocked_and_audited() {
    let cat = catalogue();
    let other = |json: &str| -> PlanStep {
        policy::validate(json, &PlanContext::new(TRANSCRIPT, &cat as &dyn Catalogue))
            .unwrap()
            .steps()[0]
            .clone()
    };
    // An injected click where the plan has a read.
    let e = env();
    *e.sup.hooks.step_override.lock().unwrap() =
        Some((1, other(&steps_json(&[&click("Refresh")]))));
    let r = e.run(&steps_json(&[OBSERVE, OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("outside_plan"), "{r:?}");
    assert_eq!(r.steps_done, 1);
    assert!(e.events().contains(&"sup_outside_plan"));
    assert_eq!(e.fake.call_count(), 0);

    // The plan's own second step run first (reordered).
    let e = env();
    *e.sup.hooks.step_override.lock().unwrap() = Some((0, other(&steps_json(&[SCROLL_UP1]))));
    let r = e.run(&steps_json(&[SCROLL1, SCROLL_UP1])).await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("outside_plan"), "{r:?}");
    assert_eq!(e.fake.call_count(), 0);
    assert!(e.rig.audit.intents().is_empty());
}

#[tokio::test]
async fn an_approval_cannot_be_replayed_by_broker_or_gate() {
    let e = env();
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    for req in e.human.seen() {
        assert_eq!(
            e.broker
                .respond(&req.id, ApprovalDecisionDto::Approve, &req.digest_hex),
            Err(crate::approval::RespondError::UnknownId)
        );
    }
    let ids: Vec<_> = e
        .rig
        .events
        .kinds()
        .into_iter()
        .filter_map(|k| match k {
            AgentEventKind::ApprovalRequested {
                approval_id,
                digest,
                ..
            } => Some((approval_id, digest)),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    for (id, digest) in ids {
        assert_eq!(
            e.rig.gate.decide(id, &digest, policy::Decision::Approve),
            Err(policy::GateError::UnknownApproval)
        );
    }
    // And a later task needs its own approvals.
    e.fake.clear_calls();
    e.human.script(&[Act::Deny]);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::LiveOffer));
    assert_eq!(e.fake.call_count(), 0);
}

#[tokio::test]
async fn an_observation_is_spent_after_one_execute_and_cannot_be_reused() {
    let e = env();
    *e.sup.hooks.reuse_observation_at.lock().unwrap() = Some(1);
    let r = e.run(&steps_json(&[SCROLL1, SCROLL_UP1])).await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("no_observation"), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Scroll), 1, "only the first step ran");
    assert_eq!(r.executed, 1);
    e.gate_idle();
}

// ---------------------------------------------------------------------------------------------
// Consequential demo: password, payment, deny list
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_password_field_target_is_never_typed_into() {
    // A field the page calls "Password": refused outright by the label rules.
    let e = env();
    let r = e
        .run(&steps_json(&[&type_into("Password", "John Smith")]))
        .await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::PlanRefusedStep),
        "{r:?}"
    );
    e.executed_nothing();
    e.gate_idle();

    // A secure field with an innocent label: it has no descriptor at all (the firewall never
    // hands it out), so the step cannot bind; the live run stops and nothing is typed.
    let desk = ScriptedDesktop::new().with_window(
        window("notes.exe").element(
            ElemSpec::new(
                21,
                NodeRole::TextField,
                "Account",
                Rect::new(200, 550, 300, 30),
            )
            .password(),
        ),
    );
    let e = env_with(Opts {
        desk,
        ..Opts::default()
    });
    let r = e
        .run(&steps_json(&[&type_into("Account", "John Smith")]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("no_such_element"), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
}

#[tokio::test]
async fn a_payment_page_is_refused_before_it_is_offered() {
    let desk = ScriptedDesktop::new().with_window(
        window("notes.exe").element(
            ElemSpec::new(
                20,
                NodeRole::TextField,
                "Card number",
                Rect::new(200, 600, 300, 30),
            )
            .editable(true),
        ),
    );
    let e = env_with(Opts {
        desk,
        ..Opts::default()
    });
    let r = e
        .run(&steps_json(&[&type_into("Name", "John Smith")]))
        .await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::PlanRefusedStep),
        "{r:?}"
    );
    assert!(e.human.seen().is_empty(), "never offered for real");
    e.executed_nothing();
    e.gate_idle();
}

#[tokio::test]
async fn a_foreground_change_after_the_first_action_stops_the_next_step() {
    // The dry run is clean; another app takes the foreground right after the first action.
    let e = env();
    let desk = e.rig.desk.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            desk.add_window(firewall::fake::WindowSpec::new(
                0x300,
                "x.exe",
                7,
                Rect::new(0, 0, 10, 10),
            ));
            desk.set_foreground(0x300);
        }
    });
    let r = e.run(&notepad_plan()).await;
    // The second step cannot run against another foreground app: stopped, nothing typed.
    assert!(
        matches!(r.outcome, TaskOutcome::Blocked(_) | TaskOutcome::Failed(_)),
        "{r:?}"
    );
    assert_eq!(e.fake.chars_sent(), 0);
    e.gate_idle();
}

// ---------------------------------------------------------------------------------------------
// Unattended origins, limits
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_scheduled_task_pauses_at_the_first_step_above_read_and_notifies() {
    let e = env();
    let r = e
        .run_as(
            RequestOrigin::Scheduled,
            &steps_json(&[OBSERVE, &click("Refresh")]),
        )
        .await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Paused(PauseReason::NeedsUser),
        "{r:?}"
    );
    assert_eq!(r.steps_done, 1, "the read ran");
    e.executed_nothing();
    assert_eq!(
        e.notes.notices(),
        vec![AgentNotice::NeedsUser { task: r.task }]
    );
    assert!(e.events().contains(&"sup_paused"));
    e.gate_idle();
}

#[tokio::test]
async fn a_manual_task_runs_reversible_steps_and_pauses_at_the_first_consequential_one() {
    let e = env();
    let r = e
        .run_as(
            RequestOrigin::Manual,
            &steps_json(&[SCROLL1, &click("Refresh")]),
        )
        .await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Paused(PauseReason::NeedsUser),
        "{r:?}"
    );
    assert_eq!(e.fake.count(CallKind::Scroll), 1);
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "the click never ran");
    assert_eq!(e.notes.notices().len(), 1);
    e.gate_idle();
    // A submit can never run unattended either.
    let r = e
        .run_as(RequestOrigin::Manual, &steps_json(&[&click("Save")]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Paused(PauseReason::NeedsUser));
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
}

#[tokio::test]
async fn schedule_source_hands_due_triggers_to_the_supervisor() {
    let e = env();
    let sched = FakeSchedule::new();
    sched.add(
        Duration::from_secs(0),
        ScheduledTrigger {
            id: 1,
            transcript: TRANSCRIPT.into(),
            every: Duration::from_secs(3600),
        },
    );
    sched.add(
        Duration::from_secs(999_999),
        ScheduledTrigger {
            id: 2,
            transcript: TRANSCRIPT.into(),
            every: Duration::from_secs(3600),
        },
    );
    e.factory.draft(&steps_json(&[OBSERVE]));
    let reports = e.sup.run_due(&sched).await;
    assert_eq!(reports.len(), 1, "only the due one ran");
    assert_eq!(reports[0].outcome, TaskOutcome::Completed);
    assert!(e.sup.run_due(&sched).await.is_empty(), "returned once");
    let m = ManualTrigger {
        transcript: "x".into(),
    }
    .into_request();
    assert_eq!(m.origin, RequestOrigin::Manual);
    assert_eq!(format!("{m:?}"), "TaskRequest(manual, 1 B)");
}

#[tokio::test]
async fn a_runaway_repeat_is_stopped_logged_and_reported() {
    let e = env_with(Opts {
        limits: Some(RunLimits {
            max_steps: 12,
            max_repeat: 2,
            time_limit: Duration::from_secs(600),
        }),
        ..Opts::default()
    });
    let r = e
        .run(&steps_json(&[SCROLL1, SCROLL1, SCROLL1, SCROLL1]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("runaway"), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Scroll), 2);
    assert!(e.events().contains(&"sup_limit"));
    assert_eq!(
        e.notes.notices(),
        vec![AgentNotice::Stopped {
            task: r.task,
            why: "runaway"
        }]
    );
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    assert!(
        e.rig.kill.epoch() > 0,
        "the limit detector tripped the switch"
    );
}

#[tokio::test]
async fn the_step_budget_and_the_time_limit_stop_the_task() {
    let e = env_with(Opts {
        limits: Some(RunLimits {
            max_steps: 2,
            max_repeat: 5,
            time_limit: Duration::from_secs(600),
        }),
        ..Opts::default()
    });
    let r = e.run(&steps_json(&[SCROLL1, SCROLL_UP1, SCROLL1])).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("step_budget"), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Scroll), 2);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));

    let e = env_with(Opts {
        limits: Some(RunLimits {
            max_steps: 12,
            max_repeat: 5,
            time_limit: Duration::from_secs(1),
        }),
        ..Opts::default()
    });
    let plan = steps_json(&[
        SCROLL1, SCROLL_UP1, SCROLL1, SCROLL_UP1, SCROLL1, SCROLL_UP1,
    ]);
    let r = e.run(&plan).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("time_limit"), "{r:?}");
    assert!(r.executed < 6);
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

#[tokio::test]
async fn unknown_facts_fail_closed_nothing_but_reads_can_run() {
    let e = env_with(Opts {
        facts: Arc::new(UnknownFacts),
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    // The dry run already shows the steps as refused (missing facts): never offered for real.
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::PlanRefusedStep),
        "{r:?}"
    );
    e.executed_nothing();
    let r = e.run(&steps_json(&[OBSERVE])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed);
}

#[tokio::test]
async fn a_locked_session_refuses_the_observation() {
    let e = env();
    e.rig.desk.set_session(SessionProbeState::LockScreen);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::ObservationUnavailable),
        "{r:?}"
    );
    e.executed_nothing();
    e.gate_idle();
}

// ---------------------------------------------------------------------------------------------
// Audit: write-ahead, and no content
// ---------------------------------------------------------------------------------------------

const CANARY: &str = "zq7canary9k";

#[tokio::test]
async fn the_audit_log_has_the_intent_first_and_no_typed_text_transcript_or_screen_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = Arc::new(::audit::FileAuditSink::open(
        dir.path(),
        ::audit::AuditOptions::default(),
    ));
    assert!(file.ready());
    let e = env_with(Opts {
        audit: Some(file.clone()),
        ..Opts::default()
    });
    let transcript = format!("open Notepad then type {CANARY} into the Name field");
    let plan = steps_json(&[OPEN_NOTEPAD, &type_into("Name", CANARY)]);
    e.factory.draft(&plan);
    let r = e
        .sup
        .run_task(TaskRequest::interactive(transcript.clone()))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(
        e.fake.chars_sent(),
        CANARY.len(),
        "the text really was typed"
    );

    let mut bytes = String::new();
    for f in std::fs::read_dir(dir.path().join("audit")).unwrap() {
        bytes.push_str(&std::fs::read_to_string(f.unwrap().path()).unwrap());
    }
    assert!(!bytes.is_empty());
    assert!(
        !bytes.contains(CANARY),
        "typed text must never reach the log"
    );
    assert!(!bytes.contains("Notepad then type"), "no transcript");
    assert!(!bytes.contains("John Smith"));
    // Write-ahead: every executed step has an intent line, BEFORE any later line of that step.
    let lines: Vec<&str> = bytes.lines().collect();
    let intents: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("\"event\":\"intent\""))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(intents.len(), 2, "one intent per executed step: {bytes}");
    assert!(lines.iter().any(|l| l.contains("sup_would_do")));
    assert!(lines.iter().any(|l| l.contains("sup_task_end")));
    // The supervisor's own records are content-free as well (fields scan on a memory run).
    let e2 = env();
    let r = e2.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed);
    let all = audit_text(&e2.rig.audit.all());
    assert!(!all.contains("John Smith") && !all.contains("Notepad then"));
    // Debug of the request and report carry no content.
    let req = TaskRequest::interactive(transcript);
    assert!(!format!("{req:?}").contains(CANARY));
    assert!(!format!("{r:?}").contains("Smith"));
}

#[test]
fn origins_map_onto_the_gates_origins_with_the_stricter_reading() {
    assert_eq!(
        RequestOrigin::Interactive.policy_origin(),
        TaskOrigin::Interactive
    );
    assert_eq!(
        RequestOrigin::Manual.policy_origin(),
        TaskOrigin::Background
    );
    assert_eq!(
        RequestOrigin::Scheduled.policy_origin(),
        TaskOrigin::Scheduled
    );
    assert!(RequestOrigin::Scheduled.unattended() && RequestOrigin::Manual.unattended());
    assert!(!RequestOrigin::Interactive.unattended());
    assert!(RunLimits::unattended().max_steps < RunLimits::interactive().max_steps);
}

#[test]
fn the_plan_fingerprint_changes_with_any_step() {
    let cat = catalogue();
    let rig = Rig::new();
    let mk = |j: &str| {
        rig.gate
            .propose(
                policy::TaskId(1),
                j,
                &PlanContext::new(TRANSCRIPT, &cat as &dyn Catalogue),
            )
            .unwrap()
    };
    let a = supervisor::fingerprint(&mk(&steps_json(&[SCROLL1])));
    let b = supervisor::fingerprint(&mk(&steps_json(&[SCROLL_UP1])));
    let a2 = supervisor::fingerprint(&mk(&steps_json(&[SCROLL1])));
    assert_ne!(a, b);
    assert_eq!(a, a2);
}
