//! ProcessEngine end to end: the REAL `buddy-agent` binary through `TestSandbox` (a plain child
//! process on Linux, no isolation), plus in-process hostile engines. Nothing here can reach a
//! gate, a driver or the firewall: the `agent` crate has no dependency on them (checked below).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent::fakes::{FakeEngine, Hostile, HostileEngine, Scripted};
use agent::sandbox::{pipe, TestSandbox};
use agent::*;
use async_trait::async_trait;

const GOOD: &str = r#"{"steps":[{"op":"open_app","app":"notepad"}],"cannot":null}"#;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_buddy-agent")
}

fn spec() -> LaunchSpec {
    LaunchSpec {
        exe: bin().into(),
        install_dir: "".into(),
    }
}

// ------------------------------------------------------------------ recording sandbox

#[derive(Default)]
struct Rec {
    sent: Mutex<Vec<u8>>,
    kills: AtomicU32,
    spawns: AtomicU32,
}

struct Tee {
    inner: Box<dyn Write + Send>,
    rec: Arc<Rec>,
}
impl Write for Tee {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.rec.sent.lock().unwrap().extend_from_slice(b);
        self.inner.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct RecProc {
    inner: Box<dyn EngineProcess>,
    rec: Arc<Rec>,
}
impl EngineProcess for RecProc {
    fn take_streams(&mut self) -> Option<ProcessStreams> {
        let s = self.inner.take_streams()?;
        Some(ProcessStreams {
            stdin: Box::new(Tee {
                inner: s.stdin,
                rec: self.rec.clone(),
            }),
            stdout: s.stdout,
            stderr: s.stderr,
        })
    }
    fn kill_handle(&self) -> KillHandle {
        let h = self.inner.kill_handle();
        let rec = self.rec.clone();
        Arc::new(move || {
            rec.kills.fetch_add(1, Ordering::SeqCst);
            h()
        })
    }
    fn wait(&mut self) -> Option<i32> {
        self.inner.wait()
    }
}

struct RecSandbox {
    inner: TestSandbox,
    rec: Arc<Rec>,
}
impl EngineSandbox for RecSandbox {
    fn spawn(&self, s: &LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError> {
        self.rec.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecProc {
            inner: self.inner.spawn(s)?,
            rec: self.rec.clone(),
        }))
    }
}

fn engine_with(inner: TestSandbox) -> (Arc<ProcessEngine>, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let sb = Arc::new(RecSandbox {
        inner,
        rec: rec.clone(),
    });
    (Arc::new(ProcessEngine::new(sb, spec(), 1)), rec)
}

fn real() -> (Arc<ProcessEngine>, Arc<Rec>) {
    engine_with(TestSandbox::real_child(bin()))
}

fn hostile(h: Hostile) -> (Arc<ProcessEngine>, Arc<Rec>) {
    engine_with(TestSandbox::in_process(HostileEngine::new(h).into_main()))
}

fn sent_lines(rec: &Rec) -> Vec<CoreToEngine> {
    let sent = rec.sent.lock().unwrap().clone();
    sent.split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| agent::protocol::Codec::decode_core(l).unwrap())
        .collect()
}
use agent::protocol::CoreToEngine;

// ------------------------------------------------------------------------- gateways

struct Gw {
    replies: Mutex<VecDeque<Result<String, GatewayError>>>,
    calls: Mutex<Vec<ModelCall>>,
}
impl Gw {
    fn new(r: Vec<Result<String, GatewayError>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(r.into()),
            calls: Mutex::new(Vec::new()),
        })
    }
    fn n(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
#[async_trait]
impl ModelGateway for Gw {
    async fn complete(&self, call: ModelCall) -> Result<String, GatewayError> {
        self.calls.lock().unwrap().push(call);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(GOOD.into()))
    }
}

/// Never answers.
struct HangingGw(AtomicU32);
#[async_trait]
impl ModelGateway for HangingGw {
    async fn complete(&self, _: ModelCall) -> Result<String, GatewayError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

fn input(t: &str) -> PlanInput {
    PlanInput {
        task_id: TaskId(5),
        transcript: Arc::from(t),
        context: SystemContext::new(),
        limits: PlanLimits::local(),
    }
}

fn draft(o: PlanOutcome) -> String {
    match o {
        PlanOutcome::Draft(d) => d.into_string(),
        o => panic!("{o:?}"),
    }
}

// ------------------------------------------------------------------ real child process

#[tokio::test]
async fn real_child_plans_and_the_protocol_recorder_sees_only_allowed_messages() {
    let (e, rec) = real();
    let gw = Gw::new(vec![Ok(GOOD.into())]);
    let out = e.plan(input("open notepad"), gw.clone()).await.unwrap();
    assert_eq!(draft(out), GOOD);
    assert_eq!(gw.n(), 1);
    let c = gw.calls.lock().unwrap()[0].clone();
    assert_eq!(c.task_id, TaskId(5));
    assert_eq!(c.purpose, CallPurpose::Plan);
    assert!(c.user.contains("open notepad"));
    // Threat row 5: core sent Hello, PlanTask, one ModelResult, then only Shutdown/Cancel.
    let sent = sent_lines(&rec);
    assert!(matches!(sent[0], CoreToEngine::Hello { v: 1, session: 1 }));
    assert!(matches!(sent[1], CoreToEngine::PlanTask(_)));
    assert!(matches!(
        sent[2],
        CoreToEngine::ModelResult { call_id: 1, .. }
    ));
    assert!(sent[3..]
        .iter()
        .all(|m| matches!(m, CoreToEngine::Shutdown {} | CoreToEngine::Cancel { .. })));
    assert_eq!(rec.spawns.load(Ordering::SeqCst), 1);
    // One process per task: the process was killed/reaped at the end.
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn real_child_repairs_through_core() {
    let (e, _) = real();
    let gw = Gw::new(vec![Ok("no".into()), Ok(GOOD.into())]);
    let out = e.plan(input("open notepad"), gw.clone()).await.unwrap();
    assert_eq!(draft(out), GOOD);
    let calls = gw.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].purpose, CallPurpose::Repair);
    assert!(calls[1].user.contains("not_json"));
}

#[tokio::test]
async fn real_child_reports_cannot_plan_and_exhaustion() {
    let (e, _) = real();
    let gw = Gw::new(vec![Ok(r#"{"steps":[],"cannot":"nope"}"#.into())]);
    assert_eq!(
        e.plan(input("sing"), gw).await.unwrap(),
        PlanOutcome::CannotPlan("cannot_plan")
    );
    let (e, _) = real();
    let gw = Gw::new(vec![Ok("a".into()), Ok("b".into()), Ok("c".into())]);
    assert_eq!(
        e.plan(input("sing"), gw.clone()).await.unwrap(),
        PlanOutcome::CannotPlan("invalid_plan")
    );
    assert_eq!(gw.n(), 3);
}

#[tokio::test]
async fn real_child_gateway_refusal_ends_the_task_with_the_gateway_error() {
    let (e, rec) = real();
    let gw = Gw::new(vec![Ok("bad".into()), Err(GatewayError::BudgetExceeded)]);
    let r = e.plan(input("x"), gw).await;
    assert_eq!(r, Err(EngineError::Gateway(GatewayError::BudgetExceeded)));
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
    for g in [
        GatewayError::LocalOnly,
        GatewayError::ConsentDenied,
        GatewayError::TooLarge,
    ] {
        let (e, _) = real();
        let r = e.plan(input("x"), Gw::new(vec![Err(g)])).await;
        assert_eq!(r, Err(EngineError::Gateway(g)));
    }
}

#[tokio::test]
async fn real_child_kill_during_a_hang_returns_quickly() {
    let (e, rec) = real();
    let gw = Arc::new(HangingGw(AtomicU32::new(0)));
    let e2 = e.clone();
    let g2 = gw.clone();
    let h = tokio::spawn(async move { e2.plan(input("x"), g2).await });
    // Wait until the child asked the model (it now blocks waiting for the answer).
    let t0 = Instant::now();
    while gw.0.load(Ordering::SeqCst) == 0 {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "child never called the gateway"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let k = Instant::now();
    e.kill();
    assert!(
        k.elapsed() < Duration::from_millis(100),
        "kill() must not block"
    );
    let r = tokio::time::timeout(Duration::from_secs(1), h)
        .await
        .expect("plan returns < 1 s")
        .unwrap();
    assert_eq!(r, Err(EngineError::Killed));
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
    // The instance is dead: no new process.
    let spawns = rec.spawns.load(Ordering::SeqCst);
    assert_eq!(
        e.plan(input("x"), Gw::new(vec![])).await,
        Err(EngineError::Killed)
    );
    assert_eq!(rec.spawns.load(Ordering::SeqCst), spawns);
}

#[tokio::test]
async fn real_child_deadline_kills_it() {
    let (e, rec) = real();
    let mut i = input("x");
    i.limits.deadline = Duration::from_millis(300);
    let t = Instant::now();
    let r = e.plan(i, Arc::new(HangingGw(AtomicU32::new(0)))).await;
    assert_eq!(r, Err(EngineError::Timeout));
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn dropping_the_future_cancels_and_kills() {
    let (e, rec) = real();
    let gw = Arc::new(HangingGw(AtomicU32::new(0)));
    let fut = e.plan(input("x"), gw.clone());
    let r = tokio::time::timeout(Duration::from_millis(500), fut).await;
    assert!(r.is_err(), "timed out, future dropped");
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
    // Cancel was sent best-effort before the kill (the writer thread may or may not win the race).
    let sent = sent_lines(&rec);
    assert!(sent
        .iter()
        .all(|m| !matches!(m, CoreToEngine::ModelResult { .. })));
    // The instance itself is still usable for a new task (cancel is not kill).
    let out = e
        .plan(input("open notepad"), Gw::new(vec![Ok(GOOD.into())]))
        .await
        .unwrap();
    assert_eq!(draft(out), GOOD);
    assert_eq!(rec.spawns.load(Ordering::SeqCst), 2, "one process per task");
}

#[tokio::test]
async fn a_second_concurrent_plan_is_refused() {
    let (e, _) = real();
    let gw = Arc::new(HangingGw(AtomicU32::new(0)));
    let e2 = e.clone();
    let g2 = gw.clone();
    let h = tokio::spawn(async move { e2.plan(input("x"), g2).await });
    let t0 = Instant::now();
    while gw.0.load(Ordering::SeqCst) == 0 {
        assert!(t0.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        e.plan(input("y"), Gw::new(vec![])).await,
        Err(EngineError::LimitExceeded("busy"))
    );
    e.kill();
    let _ = h.await;
}

#[tokio::test]
async fn spawn_failure_is_unavailable() {
    let e = ProcessEngine::new(
        Arc::new(TestSandbox::real_child("/definitely/not/here/buddy-agent")),
        spec(),
        1,
    );
    assert_eq!(
        e.plan(input("x"), Gw::new(vec![])).await,
        Err(EngineError::Unavailable("spawn_failed"))
    );
    // The busy flag was released: a second attempt reports the same error, not `busy`.
    assert_eq!(
        e.plan(input("x"), Gw::new(vec![])).await,
        Err(EngineError::Unavailable("spawn_failed"))
    );
}

#[tokio::test]
async fn unsupported_sandbox_fails_closed() {
    let e = ProcessEngine::new(Arc::new(UnsupportedSandbox), spec(), 1);
    let gw = Gw::new(vec![]);
    assert_eq!(
        e.plan(input("x"), gw.clone()).await,
        Err(EngineError::Unavailable("unsupported_platform"))
    );
    assert_eq!(gw.n(), 0);
    assert_eq!(
        SandboxError::Failed("x").to_engine_error(),
        EngineError::Unavailable("sandbox_failed")
    );
    #[cfg(not(windows))]
    {
        let p = platform_sandbox();
        assert!(matches!(p.spawn(&spec()), Err(SandboxError::Unsupported)));
    }
}

// -------------------------------------------------------------------- hostile engines

fn expected(h: Hostile) -> EngineError {
    use Hostile::*;
    match h {
        ForgedApprove | ForgedExecute | ForgedObserve | ForgedHello | UnknownField => {
            EngineError::Protocol("malformed")
        }
        OversizeLine | EndlessLine => EngineError::Protocol("line_too_long"),
        ThousandMessages | BeyondBudget => EngineError::LimitExceeded("model_calls"),
        InvalidUtf8 => EngineError::Protocol("not_utf8"),
        NulByte => EngineError::Protocol("nul_byte"),
        EmptyLine => EngineError::Protocol("empty_line"),
        WrongTaskDraft | WrongTaskModelCall => EngineError::Protocol("wrong_task"),
        DuplicateCallId => EngineError::Protocol("duplicate_call_id"),
        DraftTooLarge | OversizePrompt => EngineError::Protocol("field_too_large"),
        BadVersion => EngineError::Protocol("bad_version"),
        DraftBeforeReady => EngineError::Protocol("unexpected_message"),
        Hang => EngineError::Timeout,
    }
}

#[tokio::test]
async fn every_hostile_behaviour_ends_with_the_engine_killed_and_an_error() {
    assert!(Hostile::all().len() >= 19);
    for h in Hostile::all() {
        let (e, rec) = hostile(*h);
        let gw = Gw::new(vec![]);
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            e.plan(input("open notepad"), gw.clone()),
        )
        .await
        .unwrap_or_else(|_| panic!("{h:?} hung"));
        assert_eq!(r, Err(expected(*h)), "{h:?}");
        assert!(rec.kills.load(Ordering::SeqCst) >= 1, "{h:?} not killed");
        // NOTHING executed: the only thing the engine could ever obtain is a model answer, and
        // never more than the budget of three.
        assert!(gw.n() <= 3, "{h:?} made {} model calls", gw.n());
        // And nothing that is not a handshake/answer was ever sent to it.
        for m in sent_lines(&rec) {
            assert!(
                matches!(
                    m,
                    CoreToEngine::Hello { .. }
                        | CoreToEngine::PlanTask(_)
                        | CoreToEngine::ModelResult { .. }
                        | CoreToEngine::Cancel { .. }
                ),
                "{h:?}: {m:?}"
            );
        }
    }
}

#[tokio::test]
async fn hostile_model_call_floods_hit_the_call_limit_before_the_gateway() {
    for h in [Hostile::ThousandMessages, Hostile::BeyondBudget] {
        let (e, _) = hostile(h);
        let gw = Gw::new(vec![]);
        let _ = e.plan(input("x"), gw.clone()).await;
        assert_eq!(gw.n(), 3, "{h:?}");
    }
}

#[tokio::test]
async fn a_hanging_hostile_engine_ignores_cancel_and_is_killed_by_deadline_or_kill() {
    let (e, rec) = hostile(Hostile::Hang);
    let mut i = input("x");
    i.limits.deadline = Duration::from_millis(200);
    assert_eq!(e.plan(i, Gw::new(vec![])).await, Err(EngineError::Timeout));
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);

    let (e, rec) = hostile(Hostile::Hang);
    let e2 = e.clone();
    let h = tokio::spawn(async move { e2.plan(input("x"), Gw::new(vec![])).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let k = Instant::now();
    e.kill();
    assert!(k.elapsed() < Duration::from_millis(100));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .unwrap()
            .unwrap(),
        Err(EngineError::Killed)
    );
    assert!(rec.kills.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn an_engine_that_exits_silently_is_a_protocol_error() {
    let sb = TestSandbox::in_process(Arc::new(|_i, _o| {}));
    let (e, _) = engine_with(sb);
    assert_eq!(
        e.plan(input("x"), Gw::new(vec![])).await,
        Err(EngineError::Protocol("eof"))
    );
}

#[tokio::test]
async fn an_oversize_model_answer_is_reported_to_the_engine_not_sent() {
    // 16 KiB of control characters is a legal answer that cannot be written as one line.
    let (e, _) = real();
    let gw = Gw::new(vec![Ok("\u{1}".repeat(16 * 1024)), Ok(GOOD.into())]);
    // The child receives `too_large`, gives up, and exits without a draft: core sees EOF.
    let r = e.plan(input("x"), gw).await;
    assert_eq!(r, Err(EngineError::Protocol("eof")));
}

// ------------------------------------------------------------------------ fake engine

#[tokio::test]
async fn fake_engine_scripts_and_kill_is_prompt() {
    let f = Arc::new(FakeEngine::new(vec![
        Scripted::ok(PlanOutcome::CannotPlan("cannot_plan")),
        Scripted::err(EngineError::Timeout),
        Scripted::hang(),
    ]));
    let gw = Gw::new(vec![]);
    assert_eq!(
        f.plan(input("x"), gw.clone()).await,
        Ok(PlanOutcome::CannotPlan("cannot_plan"))
    );
    assert_eq!(
        f.plan(input("x"), gw.clone()).await,
        Err(EngineError::Timeout)
    );
    let f2 = f.clone();
    let g2 = gw.clone();
    let h = tokio::spawn(async move { f2.plan(input("x"), g2).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let k = Instant::now();
    f.kill();
    assert!(k.elapsed() < Duration::from_millis(100));
    assert_eq!(h.await.unwrap(), Err(EngineError::Killed));
    assert_eq!(f.plan(input("x"), gw).await, Err(EngineError::Killed));
    assert_eq!(f.plans.load(Ordering::SeqCst), 4);
}

// ------------------------------------------------------------------ isolation by build

#[test]
fn the_agent_crate_has_no_dependency_on_the_gate_driver_or_firewall() {
    let manifest = include_str!("../Cargo.toml");
    let deps: Vec<&str> = manifest
        .lines()
        .skip_while(|l| !l.starts_with("[dependencies]"))
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(deps.len() >= 4, "the dependency block was found");
    for banned in [
        "policy",
        "driver",
        "action",
        "capture",
        "firewall",
        "llm",
        "voice",
        "reqwest",
        "tauri",
        "buddy-core",
        "frame",
        "audit",
    ] {
        for d in &deps {
            let name = d.split('=').next().unwrap().trim();
            assert_ne!(name, banned, "agent must not depend on {banned}");
        }
    }
}

#[test]
fn in_memory_pipes_close_on_drop() {
    use std::io::Read;
    let (mut r, mut w) = pipe();
    w.write_all(b"ab").unwrap();
    drop(w);
    let mut s = String::new();
    r.read_to_string(&mut s).unwrap();
    assert_eq!(s, "ab");
}
