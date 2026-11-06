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
    /// The window cannot be shown: the broker is unavailable, the request is Denied.
    Unshowable,
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
            Act::Unshowable => return false,
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
    drv: Arc<dyn AgentDriver>,
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
    let sup = Arc::new(AgentSupervisor::new(parts).expect("audit sink"));
    sup.wire_kill();
    Env {
        rig,
        sup,
        ctl,
        drv: drv.clone(),
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
        bounded(self.sup.run_task(TaskRequest::new(origin, TRANSCRIPT))).await
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
    /// Advance the manual clock once the `nth` request (1-based) is shown AND still pending, so
    /// an earlier, already-answered request can never trigger the jump.
    fn advance_when_pending(&self, nth: usize, by: Duration) -> thread::JoinHandle<()> {
        let (b, h, clock) = (
            self.broker.clone(),
            self.human.clone(),
            self.rig.clock.clone(),
        );
        thread::spawn(move || {
            wait_until(|| h.seen().len() >= nth && b.pending_count() > 0);
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

/// Every awaited task is bounded: a removed kill hook or a lost wake-up FAILS the test with a
/// timeout instead of hanging the suite.
async fn bounded<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("timed out: the task never ended")
}

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
    let r = bounded(fut).await;
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
    let _ = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
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
    let r = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
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
        let r = bounded(e.sup.run_task(TaskRequest::interactive(t))).await;
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
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    let eng = e.factory.clone();
    wait_until(|| {
        eng.last
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|x| x.plans.load(Ordering::SeqCst) == 1)
    });
    let second = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
    assert_eq!(second.outcome, TaskOutcome::Refused(RefuseReason::Busy));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 1);
    // The running task is untouched until it is stopped.
    e.ctl.trip(KillReason::StopButton);
    let r = bounded(first).await.unwrap();
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
    let r = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::CannotPlan));
    e.factory
        .push(Scripted::err(EngineError::Protocol("bad_line")));
    let r = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
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
        let r = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
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
    let t = e.advance_when_pending(1, Duration::from_secs(130));
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
    let t = e.advance_when_pending(2, Duration::from_secs(40));
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
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    let f = e.factory.clone();
    wait_until(|| {
        f.last
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|x| x.plans.load(Ordering::SeqCst) == 1)
    });
    e.ctl.trip(KillReason::Hotkey);
    let r = bounded(task).await.unwrap();
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
    let r = bounded(e.sup.run_task(TaskRequest::interactive(transcript))).await;
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
    let reports = bounded(e.sup.run_due(&sched)).await;
    assert_eq!(reports.len(), 1, "only the due one ran");
    assert_eq!(reports[0].outcome, TaskOutcome::Completed);
    assert!(
        bounded(e.sup.run_due(&sched)).await.is_empty(),
        "returned once"
    );
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
    let r = bounded(e.sup.run_task(TaskRequest::interactive(transcript.clone()))).await;
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

// ---------------------------------------------------------------------------------------------
// S2-23b: review minors. Kill checks per phase, limits, plan input, audit sink, seal.
// ---------------------------------------------------------------------------------------------

use super::supervisor::HookPoint;

/// Trips the kill switch the `nth` time the supervisor passes `point` (1 = the first time).
fn kill_at(e: &Env, point: HookPoint, nth: u32) {
    let (ctl, hits) = (e.ctl.clone(), Arc::new(AtomicU32::new(0)));
    *e.sup.hooks.at.lock().unwrap() = Some((
        point,
        Arc::new(move || {
            if hits.fetch_add(1, Ordering::SeqCst) + 1 == nth {
                ctl.trip(KillReason::Hotkey);
            }
        }),
    ));
}

#[tokio::test]
async fn a_kill_between_attach_and_plan_never_starts_the_plan() {
    let e = env();
    kill_at(&e, HookPoint::BeforePlan, 1);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert_eq!(
        e.factory.engine().plans.load(Ordering::SeqCst),
        0,
        "plan() was never called"
    );
    assert!(
        e.factory.engine().kills.load(Ordering::SeqCst) >= 1,
        "the attached engine is killed"
    );
    assert!(e.human.seen().is_empty());
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
}

#[tokio::test]
async fn a_kill_after_propose_stops_before_the_dry_run() {
    let e = env();
    kill_at(&e, HookPoint::AfterPropose, 1);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(!r.dry_run, "no dry run was walked");
    assert!(e.human.seen().is_empty());
    assert!(!e.events().contains(&"sup_would_do"));
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
}

#[tokio::test]
async fn a_kill_before_the_approval_is_asked_asks_nothing() {
    let e = env();
    kill_at(&e, HookPoint::AfterDryRun, 1);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::PlanApproval), "{r:?}");
    assert!(r.dry_run && !r.live);
    assert!(e.human.seen().is_empty(), "the human was never asked");
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.gate_idle();
}

#[tokio::test]
async fn a_kill_at_the_start_of_the_run_loop_runs_no_step() {
    let e = env();
    kill_at(&e, HookPoint::LoopStart, 1);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(e.human.seen().len(), 1, "the plan was approved");
    assert_eq!(r.steps_done, 0);
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

#[tokio::test]
async fn a_kill_after_the_fresh_observation_mints_nothing() {
    let e = env();
    kill_at(&e, HookPoint::AfterObserve, 1);
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    e.executed_nothing();
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

#[tokio::test]
async fn a_kill_after_a_step_approval_mints_nothing_for_that_step() {
    let e = env();
    // Plan: yes. Submit: yes. The kill lands right after the SECOND pass (the click).
    e.human.script(&[Act::Approve, Act::Approve]);
    kill_at(&e, HookPoint::AfterApproval, 2);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(
        e.human.seen().len(),
        2,
        "the submit was approved by the human"
    );
    assert_eq!(e.fake.chars_sent(), "John Smith".len(), "step 0 ran");
    assert_eq!(
        e.fake.count(CallKind::Invoke),
        0,
        "the approved click never ran"
    );
    assert_eq!(e.rig.audit.intents().len(), 1, "no intent for the click");
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
}

fn scroll_limits(max_steps: u16) -> RunLimits {
    RunLimits {
        max_steps,
        max_repeat: 5,
        time_limit: Duration::from_secs(600),
    }
}

#[tokio::test]
async fn unattended_tasks_use_the_unattended_limits_and_interactive_ones_the_interactive() {
    let plan = steps_json(&[SCROLL1, SCROLL_UP1]);
    // Interactive is tight (1 step), unattended is loose (5): only the interactive task stops.
    let mk = |i, u| {
        env_with(Opts {
            limits: Some(scroll_limits(i)),
            unattended: Some(scroll_limits(u)),
            ..Opts::default()
        })
    };
    let e = mk(1, 5);
    let r = e.run_as(RequestOrigin::Interactive, &plan).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("step_budget"), "{r:?}");
    let e = mk(1, 5);
    for origin in [RequestOrigin::Manual] {
        let r = e.run_as(origin, &plan).await;
        assert_eq!(r.outcome, TaskOutcome::Completed, "{origin:?} {r:?}");
        assert_eq!(r.executed, 2);
    }
    // And the other way round.
    let e = mk(5, 1);
    let r = e.run_as(RequestOrigin::Manual, &plan).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("step_budget"), "{r:?}");
    let e = mk(5, 1);
    let r = e.run_as(RequestOrigin::Interactive, &plan).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
}

/// Records what the planner is given.
#[derive(Default)]
struct Seen {
    inputs: Mutex<Vec<(Option<String>, Duration, usize)>>,
}

struct RecEngine(Arc<Seen>, Option<Arc<CountingDeny>>);

#[async_trait]
impl ::agent::AgentEngine for RecEngine {
    fn id(&self) -> &str {
        "rec"
    }
    async fn plan(
        &self,
        input: ::agent::PlanInput,
        _models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError> {
        let calls = self
            .1
            .as_ref()
            .map_or(0, |d| d.calls.load(Ordering::SeqCst));
        self.0.inputs.lock().unwrap().push((
            input.context.foreground_app.as_deref().map(str::to_owned),
            input.limits.deadline,
            calls,
        ));
        Err(EngineError::Cancelled)
    }
    fn kill(&self) {}
}

struct RecFactory(Arc<Seen>, Option<Arc<CountingDeny>>);
impl EngineFactory for RecFactory {
    fn create(&self, _task: ::agent::TaskId) -> Result<EngineParts, EngineError> {
        Ok(EngineParts {
            engine: Arc::new(RecEngine(self.0.clone(), self.1.clone())),
            gateway: Arc::new(NoModels),
            core_gateway: None,
        })
    }
}

/// Counts `is_denied` calls; denies from call number `after` on (0-based).
struct CountingDeny {
    calls: std::sync::atomic::AtomicUsize,
    after: usize,
}
impl AppDenyPolicy for CountingDeny {
    fn is_denied(&self, _a: &str) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) >= self.after
    }
}

#[tokio::test]
async fn a_deny_listed_foreground_app_never_reaches_the_planner() {
    // FIXED expectation, not derived from the supervisor: `k` is how many times the firewall asks
    // the deny list during ONE observation (measured by observing directly). The supervisor must
    // ask exactly once more (the foreground filter) before it calls the planner, and denying that
    // very ask must blank the app. Removing the filter leaves the count at `k` and leaks the app.
    let probe = CountingDeny {
        calls: Default::default(),
        after: usize::MAX,
    };
    let e0 = env();
    let _ = e0.drv.observe(&probe);
    let k = probe.calls.load(Ordering::SeqCst);
    assert!(k >= 1);

    // Run 1: nothing denied. The planner learns the app; one ask beyond the firewall's.
    let seen = Arc::new(Seen::default());
    let deny = Arc::new(CountingDeny {
        calls: Default::default(),
        after: usize::MAX,
    });
    let e = env_with(Opts {
        deny: deny.clone(),
        factory: Some(Arc::new(RecFactory(seen.clone(), Some(deny.clone())))),
        ..Opts::default()
    });
    let r = e.run("unused").await;
    assert_eq!(
        r.outcome,
        TaskOutcome::PlanFailed("engine_cancelled"),
        "{r:?}"
    );
    let (fg, _, at_plan) = seen.inputs.lock().unwrap()[0].clone();
    assert_eq!(
        fg.as_deref(),
        Some("notes.exe"),
        "a clear, allowed app is passed"
    );
    assert_eq!(at_plan, k + 1, "the foreground filter asked the deny list");

    // Run 2: the app is denied exactly at the foreground filter (the observation itself was
    // Clear). The planner must get NO app.
    let seen = Arc::new(Seen::default());
    let deny = Arc::new(CountingDeny {
        calls: Default::default(),
        after: k,
    });
    let e = env_with(Opts {
        deny: deny.clone(),
        factory: Some(Arc::new(RecFactory(seen.clone(), Some(deny.clone())))),
        ..Opts::default()
    });
    let _ = e.run("unused").await;
    assert_eq!(
        seen.inputs.lock().unwrap()[0].0,
        None,
        "a denied app is never passed"
    );

    // A deny-listed app from the start: the screen is withheld, so none either.
    let seen = Arc::new(Seen::default());
    let e = env_with(Opts {
        deny: Arc::new(Deny(&["notes.exe"])),
        factory: Some(Arc::new(RecFactory(seen.clone(), None))),
        ..Opts::default()
    });
    let _ = e.run("unused").await;
    assert_eq!(seen.inputs.lock().unwrap()[0].0, None);
}

#[tokio::test]
async fn cloud_planning_gets_the_45s_deadline_and_local_the_60s_one() {
    for (cloud, secs) in [(true, 45), (false, 60)] {
        let seen = Arc::new(Seen::default());
        let e = env_with(Opts {
            settings: FixedSettings {
                cloud,
                ..FixedSettings::default()
            },
            factory: Some(Arc::new(RecFactory(seen.clone(), None))),
            ..Opts::default()
        });
        let _ = e.run("unused").await;
        assert_eq!(
            seen.inputs.lock().unwrap()[0].1,
            Duration::from_secs(secs),
            "cloud={cloud}"
        );
    }
}

fn test_buddy(dir: &tempfile::TempDir) -> Arc<crate::Buddy> {
    Arc::new(crate::Buddy::with_credentials(
        dir.path().join("settings.json"),
        Box::new(capture::fake::FakeCapture::new(
            capture::fake::FakeMode::ExcludeWindows,
        )),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    ))
}

struct NoProviders;
impl crate::ProviderSource for NoProviders {
    fn local(&self, _p: &str) -> Result<Arc<dyn llm::LlmProvider>, llm::LlmError> {
        Err(llm::LlmError::Config("none"))
    }
    fn cloud(&self, _m: &str) -> Result<Arc<dyn llm::LlmProvider>, llm::LlmError> {
        Err(llm::LlmError::Config("none"))
    }
}

/// A factory whose parts carry a REAL `CoreModelGateway`, so a kill can be seen reaching it.
struct GwFactory {
    buddy: Arc<crate::Buddy>,
    script: Mutex<VecDeque<Scripted>>,
    made: Mutex<Vec<Arc<crate::gateway::CoreModelGateway>>>,
}
impl EngineFactory for GwFactory {
    fn create(&self, task: ::agent::TaskId) -> Result<EngineParts, EngineError> {
        let gw = Arc::new(crate::gateway::CoreModelGateway::new(
            self.buddy.clone(),
            Arc::new(NoProviders),
            task,
        ));
        self.made.lock().unwrap().push(gw.clone());
        let s = self.script.lock().unwrap().pop_front();
        Ok(EngineParts {
            engine: Arc::new(FakeEngine::new(s.into_iter().collect())),
            gateway: gw.clone(),
            core_gateway: Some(gw),
        })
    }
}

#[tokio::test]
async fn a_kill_in_the_planning_window_kills_the_model_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let gw = Arc::new(GwFactory {
        buddy: test_buddy(&dir),
        script: Mutex::default(),
        made: Mutex::default(),
    });
    gw.script
        .lock()
        .unwrap()
        .push_back(Scripted::ok(PlanOutcome::Draft(
            PlanDraftJson::new(steps_json(&[OBSERVE])).unwrap(),
        )));
    let e = env_with(Opts {
        factory: Some(gw.clone()),
        ..Opts::default()
    });
    kill_at(&e, HookPoint::BeforePlan, 1);
    let r = e.run("unused").await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Planning), "{r:?}");
    assert!(
        gw.made.lock().unwrap()[0].is_killed(),
        "core_gateway.kill() was called"
    );
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));

    // Control: an untouched task leaves its gateway alone.
    let e = env_with(Opts {
        factory: Some(gw.clone()),
        ..Opts::default()
    });
    gw.script
        .lock()
        .unwrap()
        .push_back(Scripted::ok(PlanOutcome::Draft(
            PlanDraftJson::new(steps_json(&[OBSERVE])).unwrap(),
        )));
    let r = e.run("unused").await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert!(!gw.made.lock().unwrap()[1].is_killed());
}

#[tokio::test]
async fn an_unavailable_approval_broker_at_a_step_is_a_deny() {
    let e = env();
    // The plan is shown and approved; the submit's approval cannot be shown at all.
    e.human.script(&[Act::Approve, Act::Unshowable]);
    let r = e.run(&form_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Denied(Stage::StepApproval), "{r:?}");
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "the submit never ran");
    assert_eq!(e.human.seen().len(), 2);
    assert_eq!(e.broker.pending_count(), 0);
    e.gate_idle();
}

#[tokio::test]
async fn the_supervisor_refuses_an_audit_sink_that_is_not_the_gates() {
    let e = env();
    let parts = SupervisorParts::new(
        e.ctl.clone(),
        Arc::new(policy::MemoryAudit::new()),
        e.broker.clone(),
        e.drv.clone(),
        e.factory.clone(),
        Arc::new(FixedSettings::default()),
        Arc::new(FixedDeny(Arc::new(Deny(&[])))),
        Arc::new(catalogue()),
    );
    let err = AgentSupervisor::new(parts).unwrap_err();
    assert_eq!(err, SupervisorError::AuditMismatch);
    assert_eq!(err.code(), "audit_mismatch");
    // The gate's own sink is accepted.
    let ok = SupervisorParts::new(
        e.ctl.clone(),
        e.rig.audit.clone(),
        e.broker.clone(),
        e.drv.clone(),
        e.factory.clone(),
        Arc::new(FixedSettings::default()),
        Arc::new(FixedDeny(Arc::new(Deny(&[])))),
        Arc::new(catalogue()),
    );
    assert!(AgentSupervisor::new(ok).is_ok());
}

#[tokio::test]
async fn the_supervisors_records_land_in_the_same_log_as_the_gates_intents() {
    let e = env();
    let r = e.run(&notepad_plan()).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    // One sink holds the gate's intents AND the supervisor's would-do / end records.
    assert_eq!(e.rig.audit.intents().len(), 2);
    assert!(e.events().contains(&"sup_would_do") && e.events().contains(&"sup_task_end"));
}

#[test]
fn the_agent_driver_trait_is_sealed() {
    // The doc on `AgentDriver` states the SAFE-IF condition, and nothing outside `port.rs` can
    // implement the trait (the supertrait `Sealed` is private).
    let src = include_str!("port.rs");
    assert!(src.contains("pub trait AgentDriver: ObservationSource + sealed::Sealed"));
    assert!(src.contains("SAFE-IF"));
    assert!(src.contains("mod sealed {"));
    for other in [
        include_str!("supervisor.rs"),
        include_str!("runtime.rs"),
        include_str!("types.rs"),
        include_str!("tests.rs"),
    ] {
        assert!(
            !other.contains(&format!("impl {} for", "AgentDriver")),
            "AgentDriver is implemented only in port.rs"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// S2-23b: the glue. A real `Buddy`, its kill controller and audit sink, `AgentRuntime`.
// ---------------------------------------------------------------------------------------------

use crate::api::SettingsPatch;

/// Answers every request like a patient user, on the REAL clock (the broker's 500 ms guard).
struct SysHuman(Mutex<Option<ApprovalBroker>>);
impl ApprovalUi for SysHuman {
    fn show(&self, r: &ApprovalRequestDto) -> bool {
        let b = self.0.lock().unwrap().clone().expect("broker");
        let (id, digest) = (r.id.clone(), r.digest_hex.clone());
        thread::spawn(move || {
            b.mark_displayed(&id).unwrap();
            thread::sleep(Duration::from_millis(600));
            let _ = b.respond(&id, ApprovalDecisionDto::Approve, &digest);
        });
        true
    }
    fn close(&self, _c: &ApprovalClosedDto) {}
}

#[derive(Default)]
struct RecVoice(Mutex<Vec<String>>);
#[async_trait]
impl AgentVoice for RecVoice {
    async fn say(&self, text: String) {
        self.0.lock().unwrap().push(text);
    }
}
impl RecVoice {
    fn said(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct RecEvents(Mutex<Vec<AgentTaskDto>>);
impl crate::EventSink for RecEvents {
    fn turn_state(&self, _: crate::api::TurnState) {}
    fn transcript(&self, _: crate::api::TranscriptEvent) {}
    fn reply_delta(&self, _: crate::api::ReplyDelta) {}
    fn point(&self, _: crate::api::PointEvent) {}
    fn firewall_status(&self, _: crate::api::FirewallStatus) {}
    fn error(&self, _: crate::api::TurnError) {}
    fn agent_task(&self, ev: AgentTaskDto) {
        self.0.lock().unwrap().push(ev);
    }
}

struct Glue {
    _dir: tempfile::TempDir,
    buddy: Arc<crate::Buddy>,
    ctl: Arc<KillController>,
    rt: Arc<AgentRuntime>,
    fake: FakeActuator,
    factory: Arc<Factory>,
    voice: Arc<RecVoice>,
    events: Arc<RecEvents>,
}

fn glue(enable: bool, facts: Arc<dyn BindFacts>) -> Glue {
    let dir = tempfile::tempdir().unwrap();
    let buddy = test_buddy(&dir);
    buddy.set_data_dir(dir.path().join("data"));
    buddy.set_config_dir(dir.path().join("cfg"));
    buddy.set_kill_hotkey_registered(true);
    buddy
        .update_settings(&SettingsPatch {
            agent_enabled: Some(enable),
            observe_mode: Some(false),
            ..Default::default()
        })
        .unwrap();
    let events = Arc::new(RecEvents::default());
    buddy.set_event_sink(events.clone());
    let ctl = KillController::for_buddy(&buddy);
    let broker = ApprovalBroker::with_system_clock(ctl.gate().kill_switch());
    broker.set_ui(Arc::new(SysHuman(Mutex::new(Some(broker.clone())))));
    let desk = desktop();
    let fake = FakeActuator::new(desk.clone());
    let launch = LaunchAllowlist::empty()
        .with("notepad", "C:\\Windows\\System32\\notepad.exe")
        .unwrap();
    let drv = Arc::new(
        Driver::new(Arc::new(firewall_of(&desk)), fake.clone()).with_launch_allowlist(launch),
    );
    let factory = Arc::new(Factory::default());
    let voice = Arc::new(RecVoice::default());
    let rt = AgentRuntime::with_factory(
        &buddy,
        ctl.clone(),
        broker,
        drv,
        facts,
        factory.clone(),
        Arc::new(catalogue()),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .with_voice(voice.clone());
    Glue {
        _dir: dir,
        buddy,
        ctl,
        rt: Arc::new(rt),
        fake,
        factory,
        voice,
        events,
    }
}

fn audit_events(b: &crate::Buddy) -> Vec<String> {
    b.get_audit_entries(None, Some(500))
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.event)
        .collect()
}

#[tokio::test]
async fn a_task_runs_end_to_end_through_the_buddys_audit_sink_and_event_and_speech() {
    let g = glue(true, Arc::new(ReverifiedAtExecute));
    assert!(g.rt.status().ready, "{:?}", g.rt.status());
    g.factory.draft(&notepad_plan());
    let r = bounded(g.rt.run(TRANSCRIPT.into())).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!((r.steps_done, r.executed), (2, 2));
    assert_eq!(g.fake.count(CallKind::Launch), 1);
    assert_eq!(g.fake.chars_sent(), "John Smith".len());
    // The Buddy's own audit sink holds the gate's intents and the supervisor's end record.
    let ev = audit_events(&g.buddy);
    assert_eq!(ev.iter().filter(|e| *e == "intent").count(), 2, "{ev:?}");
    assert!(ev.iter().any(|e| e == "sup_task_end"), "{ev:?}");
    // The typed event: fixed tokens and counts.
    let dtos = g.events.0.lock().unwrap().clone();
    assert_eq!(dtos.len(), 1);
    assert_eq!(dtos[0].outcome, AgentOutcomeKind::Completed);
    assert_eq!(dtos[0].detail, "ok");
    assert_eq!((dtos[0].executed, dtos[0].live), (2, true));
    // A fixed phrase, nothing from the plan or the screen.
    assert_eq!(g.voice.said(), vec!["Done.".to_string()]);
    assert!(g.ctl.wait_idle(Duration::from_secs(10)));
}

#[tokio::test]
async fn a_kill_through_the_buddys_controller_stops_the_task_and_speaks_nothing_itself() {
    let g = glue(true, Arc::new(ReverifiedAtExecute));
    let ctl = g.ctl.clone();
    g.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            ctl.trip(KillReason::Hotkey);
        }
    });
    g.factory.draft(&notepad_plan());
    let r = bounded(g.rt.run(TRANSCRIPT.into())).await;
    assert_eq!(r.outcome, TaskOutcome::Killed(Stage::Run), "{r:?}");
    assert_eq!(g.fake.chars_sent(), 0);
    assert!(g.ctl.wait_idle(Duration::from_secs(10)));
    let ev = audit_events(&g.buddy);
    assert!(ev.iter().any(|e| e == "sup_task_end"), "{ev:?}");
    // "Stopped." is the kill controller's sentence; the runtime adds none.
    assert!(g.voice.said().is_empty(), "{:?}", g.voice.said());
    assert_eq!(
        g.events.0.lock().unwrap()[0].outcome,
        AgentOutcomeKind::Killed
    );
}

#[tokio::test]
async fn a_limit_stop_says_stopped_exactly_once() {
    let g = glue(true, Arc::new(ReverifiedAtExecute));
    let speaker = Arc::new(crate::kill::FakeSpeaker::default());
    g.ctl.set_speaker(speaker.clone());
    // Six identical steps in a row: more than the interactive repeat limit (5).
    g.factory.draft(&steps_json(&[
        SCROLL1, SCROLL1, SCROLL1, SCROLL1, SCROLL1, SCROLL1,
    ]));
    let r = bounded(g.rt.run(TRANSCRIPT.into())).await;
    assert_eq!(r.outcome, TaskOutcome::Stopped("runaway"), "{r:?}");
    assert!(g.ctl.wait_idle(Duration::from_secs(10)));
    assert_eq!(
        speaker.calls.load(Ordering::SeqCst),
        1,
        "the kill controller spoke once"
    );
    assert!(
        g.voice.said().is_empty(),
        "the runtime added none: {:?}",
        g.voice.said()
    );
}

#[tokio::test]
async fn start_task_is_rejected_unless_the_agent_is_enabled_and_idle() {
    // The user setting is OFF by default: nothing starts.
    let g = glue(false, Arc::new(UnknownFacts));
    assert_eq!(
        g.rt.start_task("open notepad".into()),
        Err(StartError::Disabled)
    );
    assert_eq!(g.rt.status().reason.as_deref(), Some("setting_off"));
    assert!(!g.rt.status().ready);
    assert_eq!(g.factory.creates.load(Ordering::SeqCst), 0);
    // On, but no stop shortcut registered: still off (fail closed).
    g.buddy
        .update_settings(&SettingsPatch {
            agent_enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    g.buddy.set_kill_hotkey_registered(false);
    assert_eq!(g.rt.start_task("x".into()), Err(StartError::Disabled));
    assert_eq!(g.rt.status().reason.as_deref(), Some("not_ready"));
    g.buddy.set_kill_hotkey_registered(true);
    assert_eq!(g.rt.start_task("  ".into()), Err(StartError::BadRequest));
    assert!(g.rt.status().ready);
    // Enabled: it starts on the runtime and the outcome arrives as an event.
    g.factory.draft(&steps_json(&[OBSERVE]));
    g.rt.start_task(TRANSCRIPT.into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while g.events.0.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the task never ended");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        g.events.0.lock().unwrap()[0].outcome,
        AgentOutcomeKind::Completed
    );
}

#[tokio::test]
async fn from_buddy_without_a_sandbox_refuses_with_sandbox_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let buddy = test_buddy(&dir);
    buddy.set_data_dir(dir.path().join("data"));
    buddy.set_config_dir(dir.path().join("cfg"));
    buddy.set_kill_hotkey_registered(true);
    buddy
        .update_settings(&SettingsPatch {
            agent_enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    let ctl = KillController::for_buddy(&buddy);
    let broker = ApprovalBroker::with_system_clock(ctl.gate().kill_switch());
    let desk = desktop();
    let platform = PlatformAgent {
        driver: Arc::new(Driver::new(
            Arc::new(firewall_of(&desk)),
            FakeActuator::new(desk.clone()),
        )),
        facts: Arc::new(UnknownFacts),
        sandbox: Arc::new(::agent::UnsupportedSandbox),
        spec: LaunchSpec {
            exe: "/opt/hey_buddy/buddy-agent".into(),
            install_dir: "/opt/hey_buddy".into(),
        },
        catalogue: Arc::new(catalogue()),
    };
    let rt = AgentRuntime::from_buddy(
        &buddy,
        ctl,
        broker,
        platform,
        Arc::new(NoProviders),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .with_voice(Arc::new(RecVoice::default()));
    let r = bounded(rt.run(TRANSCRIPT.into())).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::SandboxUnavailable),
        "{r:?}"
    );
    assert_eq!(sentences_for(&r), vec!["I couldn't do that.".to_string()]);
}

#[test]
fn the_platform_wiring_is_read_only_off_windows_and_never_picks_reverified_facts_there() {
    let desk = desktop();
    let p = platform_agent(
        std::path::Path::new("/opt/hey_buddy"),
        Arc::new(firewall_of(&desk)),
    );
    #[cfg(not(windows))]
    {
        let p = p.unwrap();
        // Every fact unknown: only reads can run (D-009).
        let obs = observe_of(&desk);
        let steps = [];
        let x = p.facts.extras(&steps, &obs, &Deny(&[]));
        assert!(
            x.higher_integrity.is_none() && x.focused_secure.is_none() && x.target_denied.is_none()
        );
        assert!(matches!(
            p.sandbox.spawn(&p.spec),
            Err(::agent::SandboxError::Unsupported)
        ));
    }
    #[cfg(windows)]
    let _ = p;
    // The facts are chosen in exactly one non-test place, under cfg(windows).
    let src = include_str!("runtime.rs");
    let body = src.split("#[cfg(test)]").next().unwrap();
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let uses: Vec<_> = code.match_indices("ReverifiedAtExecute").collect();
    assert_eq!(uses.len(), 1, "one use outside tests");
    let before = &code[..uses[0].0];
    assert!(
        before.rfind("#[cfg(windows)]").unwrap()
            > before.rfind("#[cfg(not(windows))]").unwrap_or(0),
        "ReverifiedAtExecute sits in the cfg(windows) function"
    );
}

#[test]
fn spoken_sentences_are_fixed_or_made_of_cleared_labels_only() {
    let rep = |o| {
        let mut r = TaskReport::refused(RefuseReason::Busy);
        r.outcome = o;
        r
    };
    let fixed = |o| sentences_for(&rep(o));
    assert_eq!(fixed(TaskOutcome::Completed), vec!["Done."]);
    // A limit stop goes through the kill controller, which says "Stopped." itself: exactly once.
    assert!(fixed(TaskOutcome::Stopped("runaway")).is_empty());
    assert!(fixed(TaskOutcome::Killed(Stage::Run)).is_empty());
    assert_eq!(
        fixed(TaskOutcome::Denied(Stage::LiveOffer)),
        vec!["Okay, I won't."]
    );
    assert_eq!(
        fixed(TaskOutcome::Paused(PauseReason::NeedsUser)),
        vec!["I need you to take a look."]
    );
    for o in [
        TaskOutcome::Refused(RefuseReason::Busy),
        TaskOutcome::PlanFailed("engine_protocol"),
        TaskOutcome::PlanRejected("unknown_op"),
        TaskOutcome::Blocked("outside_plan"),
        TaskOutcome::Failed("platform"),
    ] {
        assert_eq!(fixed(o), vec!["I couldn't do that."], "{o:?}");
    }
    // The DTO carries tokens and counts; Debug is content-free.
    let d = AgentTaskDto::from(&rep(TaskOutcome::Blocked("outside_plan")));
    assert_eq!(
        (d.outcome, d.detail.as_str()),
        (AgentOutcomeKind::Blocked, "outside_plan")
    );
    assert!(!format!("{d:?}").contains("Smith"));
}

#[tokio::test]
async fn find_text_results_are_read_from_cleared_screen_text_and_spoken() {
    let g = glue(true, Arc::new(UnknownFacts));
    // A read-only plan runs even with unknown facts. The query is the engine's; only matching
    // cleared labels are quoted. "Password" is a secure field: the firewall hides its label.
    g.factory.draft(&steps_json(&[
        r#"{"op":"find_text","query":"save"}"#,
        r#"{"op":"find_text","query":"zzz-not-there"}"#,
    ]));
    let r = bounded(g.rt.run(TRANSCRIPT.into())).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(r.answers.len(), 2);
    assert_eq!(r.answers[0].matches(), 1);
    assert_eq!(r.answers[0].first_label(), "Save");
    assert_eq!(r.answers[0].first_role(), "button");
    assert_eq!(r.answers[1].matches(), 0);
    assert_eq!(
        g.voice.said(),
        vec![
            "I found one match. The first is a button: Save.".to_string(),
            "I could not find that on the screen.".to_string()
        ]
    );
    // Debug never prints the label; the DTO carries it for the (capture-protected) window.
    assert!(!format!("{:?}", r.answers[0]).contains("Save"));
    let dto = AgentTaskDto::from(&r);
    assert_eq!(dto.answers[0].first_label, "Save");
    assert_eq!(g.fake.call_count(), 0, "reads never reach the driver");
}
