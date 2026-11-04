//! `CoreModelGateway` and `ProcessEngineKiller` (S2-10/15) on fakes only: real Ollama and
//! Anthropic providers over `FakeTransport`, the real `Buddy` consent flow, a scripted preview
//! sink. No network, no devices.

#[path = "../../policy/tests/common/world.rs"]
mod world;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::fakes::{minimal_main, FakeEngine, Scripted};
use agent::sandbox::TestSandbox;
use agent::{
    AgentEngine, CallPurpose, EngineError, GatewayError, LaunchSpec, ModelCall, ModelGateway,
    OutputSchema, PlanInput, PlanLimits, PlanOutcome, ProcessEngine, SystemContext, TaskId,
};
use buddy_core::api::{Decision, ProviderChoice, PurposeDto, SettingsPatch};
use buddy_core::consent::PreviewSink;
use buddy_core::gateway::{CoreModelGateway, ProcessEngineKiller, MAX_CALLS};
use buddy_core::kill::{EngineKiller, KillController, KillReason, KillTimeouts};
use buddy_core::{Buddy, ProviderSource};
use llm::fakes::{FakeCredentialStore, FakeTransport};
use llm::{
    AnthropicConfig, AnthropicProvider, LlmError, LlmProvider, OllamaConfig, OllamaProvider,
    ANTHROPIC_ACCOUNT,
};

const TASK: TaskId = TaskId(5);
const PLAN: &str = r#"{"steps":[{"op":"open_app","app":"notepad"}],"cannot":null}"#;

fn ndjson(text: &str, done_reason: Option<&str>) -> Vec<Vec<u8>> {
    let mut v = vec![{
        let mut s =
            serde_json::json!({"message": {"role": "assistant", "content": text}, "done": false})
                .to_string();
        s.push('\n');
        s.into_bytes()
    }];
    if let Some(r) = done_reason {
        v.push(format!("{{\"done\":true,\"done_reason\":\"{r}\"}}\n").into_bytes());
    }
    v
}

fn sse(text: &str) -> Vec<Vec<u8>> {
    vec![
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})
        )
        .into_bytes(),
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n".to_vec(),
        b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec(),
    ]
}

struct Providers {
    local: Arc<OllamaProvider>,
    cloud: Arc<AnthropicProvider>,
}

impl ProviderSource for Providers {
    fn local(&self, _profile: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        Ok(self.local.clone())
    }
    fn cloud(&self, _model: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        Ok(self.cloud.clone())
    }
}

#[derive(Default)]
struct Sink {
    shown: Mutex<Vec<buddy_core::api::PreviewDto>>,
}
impl PreviewSink for Sink {
    fn show(&self, dto: buddy_core::api::PreviewDto) -> bool {
        self.shown.lock().unwrap().push(dto);
        true
    }
    fn close(&self, _id: u64) {}
}

struct Rig {
    _dir: tempfile::TempDir,
    buddy: Arc<Buddy>,
    sink: Arc<Sink>,
    local_t: Arc<FakeTransport>,
    cloud_t: Arc<FakeTransport>,
    providers: Arc<Providers>,
    /// What the "user" answers to the next previews (cycled); `None` = never answers.
    decide: Arc<Mutex<Option<Decision>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn rig(
    local_only: bool,
    provider: ProviderChoice,
    local_text: &str,
    local_done: Option<&str>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let creds = Arc::new(FakeCredentialStore::with_key(ANTHROPIC_ACCOUNT, "k-test"));
    let buddy = Arc::new(Buddy::with_credentials(
        dir.path().join("s.json"),
        Box::new(capture::UnsupportedCapture),
        creds.clone(),
    ));
    buddy
        .update_settings(&SettingsPatch {
            local_only: Some(local_only),
            provider: Some(provider),
            anthropic_model: Some("test-model".into()),
            ..Default::default()
        })
        .unwrap();
    let sink = Arc::new(Sink::default());
    buddy.set_preview_sink(sink.clone());
    let local_t = Arc::new(FakeTransport::responding(
        200,
        ndjson(local_text, local_done),
    ));
    let cloud_t = Arc::new(FakeTransport::responding(200, sse(PLAN)));
    let providers = Arc::new(Providers {
        local: Arc::new(
            OllamaProvider::with_transport(OllamaConfig::default(), local_t.clone()).unwrap(),
        ),
        cloud: Arc::new(
            AnthropicProvider::with_transport(
                AnthropicConfig::new("test-model"),
                cloud_t.clone(),
                creds,
            )
            .unwrap(),
        ),
    });
    let decide = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    // The "user": answers every preview that appears.
    {
        let (b, s, d, stop) = (buddy.clone(), sink.clone(), decide.clone(), stop.clone());
        tokio::spawn(async move {
            let mut answered = 0usize;
            while !stop.load(Ordering::SeqCst) {
                let id = s.shown.lock().unwrap().get(answered).map(|x| x.request_id);
                let dec = *d.lock().unwrap();
                if let (Some(id), Some(dec)) = (id, dec) {
                    let _ = b.preview_decision(id, dec);
                    answered += 1;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
    }
    Rig {
        _dir: dir,
        buddy,
        sink,
        local_t,
        cloud_t,
        providers,
        decide,
        stop,
    }
}

fn local_rig() -> Rig {
    rig(true, ProviderChoice::Ollama, PLAN, Some("stop"))
}

fn gw(r: &Rig) -> CoreModelGateway {
    CoreModelGateway::new(r.buddy.clone(), r.providers.clone(), TASK)
}

fn call(purpose: CallPurpose) -> ModelCall {
    ModelCall {
        task_id: TASK,
        purpose,
        system: "SYSTEM_TEXT".into(),
        user: "{\"transcript\":\"open notepad\"}".into(),
        schema: OutputSchema::PlanV1,
    }
}

// --------------------------------------------------------------------------------- local

#[tokio::test]
async fn local_call_needs_no_preview_and_is_text_only_with_the_schema() {
    let r = local_rig();
    let g = gw(&r);
    assert_eq!(g.complete(call(CallPurpose::Plan)).await.unwrap(), PLAN);
    assert_eq!(r.sink.shown.lock().unwrap().len(), 0, "local: no preview");
    let calls = r.local_t.calls();
    assert_eq!(calls.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&calls[0].body).unwrap();
    assert_eq!(body["format"], *agent::plan_v1_schema());
    assert_eq!(body["options"]["temperature"].as_f64().unwrap(), 0.0);
    assert_eq!(body["messages"][0]["content"], "SYSTEM_TEXT");
    // Text only: the user message is exactly the engine's text (no <screen> block, no image).
    assert_eq!(
        body["messages"][1]["content"],
        "{\"transcript\":\"open notepad\"}"
    );
    assert!(body["messages"][1].get("images").is_none());
    assert_eq!(g.calls_used(), 1);
    // The last-request record says it was agent planning.
    let last = r.buddy.get_last_request().unwrap();
    assert_eq!(last.purpose, PurposeDto::AgentPlan);
}

#[tokio::test]
async fn the_budget_is_three_calls_and_refusals_count() {
    let r = local_rig();
    let g = gw(&r);
    for _ in 0..MAX_CALLS {
        g.complete(call(CallPurpose::Plan)).await.unwrap();
    }
    assert_eq!(
        g.complete(call(CallPurpose::Repair)).await,
        Err(GatewayError::BudgetExceeded)
    );
    assert_eq!(r.local_t.call_count(), 3);
    // A fresh task gets a fresh budget; refused attempts (too large, wrong task) spend one.
    let g = gw(&r);
    let mut big = call(CallPurpose::Plan);
    big.system = "s".repeat(6 * 1024 + 1);
    assert_eq!(g.complete(big).await, Err(GatewayError::TooLarge));
    let mut other = call(CallPurpose::Plan);
    other.task_id = TaskId(6);
    assert_eq!(
        g.complete(other).await,
        Err(GatewayError::Model("wrong_task"))
    );
    g.complete(call(CallPurpose::Plan)).await.unwrap();
    assert_eq!(
        g.complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::BudgetExceeded)
    );
    assert_eq!(r.local_t.call_count(), 4, "three before + one now");
}

#[tokio::test]
async fn size_caps_make_zero_transport_calls() {
    let r = local_rig();
    let g = gw(&r);
    for (system, user) in [(6 * 1024 + 1, 1), (1, 6 * 1024 + 1)] {
        let mut c = call(CallPurpose::Plan);
        c.system = "s".repeat(system);
        c.user = "u".repeat(user);
        assert_eq!(g.complete(c).await, Err(GatewayError::TooLarge));
    }
    assert_eq!(r.local_t.call_count(), 0);
    // Exactly at the cap is fine.
    let g = gw(&r);
    let mut c = call(CallPurpose::Plan);
    c.system = "s".repeat(6 * 1024);
    c.user = "u".repeat(6 * 1024);
    assert!(g.complete(c).await.is_ok());
}

#[tokio::test]
async fn an_oversize_response_is_refused_and_a_truncated_one_is_not_returned() {
    let r = rig(
        true,
        ProviderChoice::Ollama,
        &"x".repeat(16 * 1024 + 1),
        Some("stop"),
    );
    assert_eq!(
        gw(&r).complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::TooLarge)
    );
    let r = rig(
        true,
        ProviderChoice::Ollama,
        &"x".repeat(16 * 1024),
        Some("stop"),
    );
    assert_eq!(
        gw(&r)
            .complete(call(CallPurpose::Plan))
            .await
            .unwrap()
            .len(),
        16 * 1024
    );
    // Hit the token limit: a partial plan is never handed back.
    let r = rig(true, ProviderChoice::Ollama, "{\"steps\":[", Some("length"));
    assert_eq!(
        gw(&r).complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::Model("truncated"))
    );
    // Real parser: a stream that ends without a done marker is an error, never a partial reply.
    let r = rig(true, ProviderChoice::Ollama, "{\"steps\":[", None);
    assert!(gw(&r).complete(call(CallPurpose::Plan)).await.is_err());
}

#[tokio::test]
async fn a_clean_end_of_stream_without_done_is_truncated() {
    let r = local_rig();
    let provider = Arc::new(OneProvider(Arc::new(llm::fakes::FakeLlm::local(vec![
        llm::LlmDelta::Text("{\"steps\":[".into()),
    ]))));
    let g = CoreModelGateway::new(r.buddy.clone(), provider, TASK);
    assert_eq!(
        g.complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::Model("truncated"))
    );
}

/// Local provider whose stream flips the gateway's kill flag between two deltas.
struct KillBetweenDeltas {
    inner: llm::fakes::FakeLlm,
    gw: Arc<std::sync::OnceLock<Arc<CoreModelGateway>>>,
}

#[async_trait::async_trait]
impl LlmProvider for KillBetweenDeltas {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> llm::Locality {
        self.inner.locality()
    }
    fn prepare(&self, req: &llm::LlmRequest) -> Result<llm::PreparedRequest, LlmError> {
        self.inner.prepare(req)
    }
    async fn stream(
        &self,
        _req: llm::PreparedRequest,
        _opt_in: Option<llm::CloudOptIn>,
    ) -> Result<futures::stream::BoxStream<'static, Result<llm::LlmDelta, LlmError>>, LlmError>
    {
        use futures::StreamExt;
        let gw = self.gw.clone();
        Ok(futures::stream::iter(0..3)
            .map(move |i| {
                if i == 1 {
                    gw.get().unwrap().kill();
                }
                Ok(match i {
                    0 => llm::LlmDelta::Text("{".into()),
                    1 => llm::LlmDelta::Text("}".into()),
                    _ => llm::LlmDelta::Done {
                        stop: llm::StopReason::EndTurn,
                    },
                })
            })
            .boxed())
    }
}

struct OneProvider(Arc<dyn LlmProvider>);
impl ProviderSource for OneProvider {
    fn local(&self, _profile: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        Ok(self.0.clone())
    }
    fn cloud(&self, _model: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        Err(LlmError::Config("no_cloud"))
    }
}

#[tokio::test]
async fn a_kill_between_deltas_is_killed_not_a_partial_reply() {
    let r = local_rig();
    let cell = Arc::new(std::sync::OnceLock::new());
    let provider = Arc::new(OneProvider(Arc::new(KillBetweenDeltas {
        inner: llm::fakes::FakeLlm::local(vec![]),
        gw: cell.clone(),
    })));
    let g = Arc::new(CoreModelGateway::new(r.buddy.clone(), provider, TASK));
    assert!(cell.set(g.clone()).is_ok());
    assert_eq!(
        g.complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::Killed)
    );
}

#[tokio::test]
async fn killed_gateway_refuses() {
    let r = local_rig();
    let g = gw(&r);
    g.kill();
    assert_eq!(
        g.complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::Killed)
    );
    assert_eq!(r.local_t.call_count(), 0);
}

// ---------------------------------------------------------------------------- local_only

#[tokio::test]
async fn local_only_refuses_cloud_without_preview_or_transport() {
    let r = rig(true, ProviderChoice::Anthropic, PLAN, Some("stop"));
    assert_eq!(
        gw(&r).complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::LocalOnly)
    );
    assert_eq!(r.sink.shown.lock().unwrap().len(), 0);
    assert_eq!(r.cloud_t.call_count(), 0);
    assert_eq!(r.local_t.call_count(), 0);
}

// ------------------------------------------------------------------------------- cloud

#[tokio::test]
async fn cloud_without_send_is_consent_denied_with_zero_transport_calls() {
    let r = rig(false, ProviderChoice::Anthropic, PLAN, Some("stop"));
    *r.decide.lock().unwrap() = Some(Decision::Cancel);
    assert_eq!(
        gw(&r).complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::ConsentDenied)
    );
    assert_eq!(r.cloud_t.call_count(), 0);
    // The preview said what it was.
    let shown = r.sink.shown.lock().unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].purpose, PurposeDto::AgentPlan);
    assert_eq!(
        shown[0].screen_text, None,
        "no screen text in an agent request"
    );
    assert!(shown[0].image_url.is_none());
}

#[tokio::test]
async fn cloud_with_send_makes_exactly_one_call_and_each_repair_asks_again() {
    let r = rig(false, ProviderChoice::Anthropic, PLAN, Some("stop"));
    *r.decide.lock().unwrap() = Some(Decision::Send);
    let g = gw(&r);
    assert_eq!(g.complete(call(CallPurpose::Plan)).await.unwrap(), PLAN);
    assert_eq!(r.cloud_t.call_count(), 1);
    assert_eq!(r.sink.shown.lock().unwrap().len(), 1);
    // The sent bytes are the previewed bytes (body served by the store == transport body).
    let body: serde_json::Value = serde_json::from_slice(&r.cloud_t.calls()[0].body).unwrap();
    assert!(body["system"].as_str().unwrap().contains("plan_v1"));
    // The repair asks again: a second preview, and with no answer, no second call.
    *r.decide.lock().unwrap() = None;
    let h = tokio::time::timeout(
        Duration::from_millis(300),
        g.complete(call(CallPurpose::Repair)),
    )
    .await;
    assert!(h.is_err(), "still waiting for the user");
    assert_eq!(
        r.sink.shown.lock().unwrap().len(),
        2,
        "the repair asked again"
    );
    assert_eq!(r.cloud_t.call_count(), 1, "nothing sent without a Send");
    r.buddy.cancel_pending_consents();
    // Cancel the repair: refused.
    let g2 = gw(&r);
    *r.decide.lock().unwrap() = Some(Decision::Cancel);
    g2.complete(call(CallPurpose::Plan)).await.unwrap_err();
    assert_eq!(r.cloud_t.call_count(), 1);
}

#[tokio::test]
async fn cloud_missing_key_or_model_is_a_fixed_token() {
    // No model name.
    let r = rig(false, ProviderChoice::Anthropic, PLAN, Some("stop"));
    r.buddy
        .update_settings(&SettingsPatch {
            anthropic_model: Some("".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        gw(&r).complete(call(CallPurpose::Plan)).await,
        Err(GatewayError::Model("cloud_not_configured"))
    );
    assert_eq!(r.cloud_t.call_count(), 0);
}

// ----------------------------------------------------------- engine through the gateway

fn input(t: &str) -> PlanInput {
    PlanInput {
        task_id: TASK,
        transcript: Arc::from(t),
        context: SystemContext::new(),
        limits: PlanLimits::local(),
    }
}

#[tokio::test]
async fn minimal_engine_child_plans_through_the_core_gateway() {
    let r = local_rig();
    let engine = ProcessEngine::new(
        Arc::new(TestSandbox::in_process(minimal_main())),
        LaunchSpec {
            exe: "".into(),
            install_dir: "".into(),
        },
        1,
    );
    let g = Arc::new(gw(&r));
    let out = engine.plan(input("open notepad"), g.clone()).await.unwrap();
    match out {
        PlanOutcome::Draft(d) => assert_eq!(d.as_str(), PLAN),
        o => panic!("{o:?}"),
    }
    let body: serde_json::Value = serde_json::from_slice(&r.local_t.calls()[0].body).unwrap();
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("open notepad"));
    assert_eq!(g.calls_used(), 1);
}

#[tokio::test]
async fn exhausted_repairs_use_exactly_three_gateway_calls() {
    // A model that never gives a valid plan: the engine repairs twice and gives up; the gateway
    // saw exactly three calls.
    let r = rig(true, ProviderChoice::Ollama, "not json", Some("stop"));
    let engine = ProcessEngine::new(
        Arc::new(TestSandbox::in_process(minimal_main())),
        LaunchSpec {
            exe: "".into(),
            install_dir: "".into(),
        },
        1,
    );
    let g = Arc::new(gw(&r));
    let out = engine.plan(input("open notepad"), g.clone()).await.unwrap();
    assert_eq!(out, PlanOutcome::CannotPlan("invalid_plan"));
    assert_eq!(r.local_t.call_count(), 3);
}

// -------------------------------------------------------------------------- the killer

#[test]
fn process_engine_killer_calls_kill_and_is_safe_without_an_engine() {
    let k = ProcessEngineKiller::new();
    k.kill_engine(); // nothing attached: no-op
    let e = Arc::new(FakeEngine::new(vec![]));
    k.attach(e.clone());
    k.kill_engine();
    assert_eq!(e.kills.load(Ordering::SeqCst), 1);
    k.detach();
    k.kill_engine();
    assert_eq!(e.kills.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_kill_switch_kills_the_engine_through_set_engine_killer() {
    let rig = world::Rig::new();
    let ctl = KillController::with_timeouts(
        rig.gate.clone(),
        KillTimeouts {
            step: Duration::from_millis(400),
            speak: Duration::from_millis(400),
        },
    );
    let engine = Arc::new(FakeEngine::new(vec![Scripted::hang()]));
    let killer = Arc::new(ProcessEngineKiller::new());
    killer.attach(engine.clone());
    ctl.set_engine_killer(killer.clone());
    let e2 = engine.clone();
    let r = local_rig();
    let g = Arc::new(gw(&r));
    let h = tokio::spawn(async move { e2.plan(input("x"), g).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let t = std::time::Instant::now();
    ctl.trip(KillReason::Hotkey);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .unwrap()
            .unwrap(),
        Err(EngineError::Killed)
    );
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(engine.kills.load(Ordering::SeqCst), 1);
    assert!(ctl.wait_idle(Duration::from_secs(20)));
}
