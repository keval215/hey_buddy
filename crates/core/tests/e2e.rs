//! End-to-end turns on fakes only (Linux CI): synthetic login form from `FakeCapture`, the REAL
//! `PrivacyFirewall` over fake detector / session probe / text source, fake mic / STT / TTS /
//! speaker, and the REAL Ollama / Anthropic providers over `FakeTransport`. No network, no
//! devices, no models.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use buddy_core::api::{
    Decision, FirewallState, FirewallStatus, LocalityDto, Phase, PointEvent, PreviewDto,
    ProviderChoice, ReplyDelta, SettingsPatch, TranscriptEvent, TurnError, TurnState, VisionMode,
};
use buddy_core::consent::{AssetKind, PreviewSink, PreviewStore};
use buddy_core::providers::ollama_models;
use buddy_core::spoken::{self, Locale, NotShared, Notice};
use buddy_core::{Buddy, EngineDeps, EventSink, ProviderSource};
use bytes::Bytes;
use capture::fake::{
    FakeCapture, FakeMode, FIELD_COLOR, PASSWORD_COLOR, PASSWORD_FIELD, USERNAME_FIELD,
};
use capture::{CaptureError, CaptureRequest, CaptureSource, DenyList};
use firewall::fake::{FakeDetector, FakeSessionProbe, FakeTextSource};
use firewall::{
    Firewall, FirewallError, NodeRole, PauseReason, PrivacyFirewall, RawTextNode, RawWindowTree,
    SessionProbeState,
};
use frame::{DisplayInfo, Frame, Rect};
use futures::stream::{self, BoxStream, StreamExt};
use llm::fakes::{FakeCredentialStore, FakeTransport};
use llm::{
    AnthropicConfig, AnthropicProvider, LlmError, LlmProvider, OllamaConfig, OllamaProvider,
    ANTHROPIC_ACCOUNT, DEFAULT_OLLAMA_BASE_URL,
};
use voice::fake::{FakeAudioIn, FakeAudioOut, FakeStt, FakeTts, FakeVad};
use voice::{
    AudioChunk, AudioInput, AudioOutput, PlaybackHandle, SttEvent, SttOptions, SttProvider,
    TtsOptions, TtsProvider, Vad, VoiceError,
};

const MARKER: &str = "PRIVATE_MARKER_8431";
const QUESTION: &str = "what does this button do?";
const BUTTON: Rect = Rect::new(300, 360, 200, 36);

// ------------------------------------------------------------------ recording event sink

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    State(TurnState),
    Transcript(TranscriptEvent),
    Reply(ReplyDelta),
    Point(PointEvent),
    Firewall(FirewallStatus),
    Error(TurnError),
}

#[derive(Default)]
struct Rec(Mutex<Vec<Ev>>);

impl Rec {
    fn push(&self, e: Ev) {
        self.0.lock().unwrap().push(e);
    }
    fn all(&self) -> Vec<Ev> {
        self.0.lock().unwrap().clone()
    }
    fn phases(&self) -> Vec<Phase> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::State(s) => Some(s.phase),
                _ => None,
            })
            .collect()
    }
    fn points(&self) -> Vec<PointEvent> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Point(p) => Some(p),
                _ => None,
            })
            .collect()
    }
    fn errors(&self) -> Vec<TurnError> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Error(p) => Some(p),
                _ => None,
            })
            .collect()
    }
    fn firewall(&self) -> Vec<FirewallStatus> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Firewall(p) => Some(p),
                _ => None,
            })
            .collect()
    }
    fn reply_text(&self) -> String {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Reply(r) => Some(r.text),
                _ => None,
            })
            .collect()
    }
    fn transcripts(&self) -> Vec<String> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Transcript(r) => Some(r.text),
                _ => None,
            })
            .collect()
    }
}

impl EventSink for Rec {
    fn turn_state(&self, ev: TurnState) {
        self.push(Ev::State(ev));
    }
    fn transcript(&self, ev: TranscriptEvent) {
        self.push(Ev::Transcript(ev));
    }
    fn reply_delta(&self, ev: ReplyDelta) {
        self.push(Ev::Reply(ev));
    }
    fn point(&self, ev: PointEvent) {
        self.push(Ev::Point(ev));
    }
    fn firewall_status(&self, st: FirewallStatus) {
        self.push(Ev::Firewall(st));
    }
    fn error(&self, e: TurnError) {
        self.push(Ev::Error(e));
    }
}

// ------------------------------------------------------------------ instrumented fakes

/// Counts capture calls; delegates to the synthetic login screen.
struct CountingCapture {
    inner: FakeCapture,
    captures: Arc<AtomicUsize>,
}

#[async_trait]
impl CaptureSource for CountingCapture {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        self.inner.displays()
    }
    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        self.inner.capture(req).await
    }
}

/// Counts opens and live streams; optionally never ends (a held key).
struct CountMic {
    inner: FakeAudioIn,
    opens: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    endless: bool,
}

struct LiveGuard(Arc<AtomicUsize>);
impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AudioInput for CountMic {
    fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError> {
        let s = self.inner.open_push_to_talk()?;
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.live.fetch_add(1, Ordering::SeqCst);
        let guard = LiveGuard(self.live.clone());
        // The guard rides on the OUTER stream, so it lives exactly as long as the stream does.
        let s: BoxStream<'static, AudioChunk> = if self.endless {
            s.chain(stream::pending()).boxed()
        } else {
            s
        };
        Ok(s.map(move |c| {
            let _keep = &guard;
            c
        })
        .boxed())
    }
}

/// Records every text piece TTS receives.
struct RecTts {
    inner: FakeTts,
    inputs: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl TtsProvider for RecTts {
    fn id(&self) -> &str {
        "rec-tts"
    }
    async fn synthesize(
        &self,
        text: BoxStream<'static, String>,
        opts: TtsOptions,
    ) -> Result<BoxStream<'static, Result<AudioChunk, VoiceError>>, VoiceError> {
        let inputs = self.inputs.clone();
        let text = text
            .inspect(move |t| inputs.lock().unwrap().push(t.clone()))
            .boxed();
        self.inner.synthesize(text, opts).await
    }
}

/// Playback that never finishes by itself and records `stop`.
struct StopOut {
    stopped: Arc<AtomicBool>,
    plays: Arc<AtomicUsize>,
}

impl AudioOutput for StopOut {
    fn play(
        &self,
        audio: BoxStream<'static, Result<AudioChunk, VoiceError>>,
    ) -> Result<PlaybackHandle, VoiceError> {
        self.plays.fetch_add(1, Ordering::SeqCst);
        let stopped = self.stopped.clone();
        // Keep the audio stream alive inside the handle.
        let hold = Arc::new(Mutex::new(Some(audio)));
        let hold2 = hold.clone();
        Ok(PlaybackHandle::new(
            move || {
                stopped.store(true, Ordering::SeqCst);
                hold2.lock().unwrap().take();
            },
            Box::pin(async move {
                let _hold = hold;
                futures::future::pending::<()>().await;
            }),
        ))
    }
}

/// STT that fails like a missing model file.
struct MissingStt;

#[async_trait]
impl SttProvider for MissingStt {
    fn id(&self) -> &str {
        "missing-stt"
    }
    async fn transcribe(
        &self,
        _audio: BoxStream<'static, AudioChunk>,
        _opts: SttOptions,
    ) -> Result<BoxStream<'static, Result<SttEvent, VoiceError>>, VoiceError> {
        Err(VoiceError::ModelMissing {
            path: PathBuf::from("/models/whisper-small.bin"),
        })
    }
}

struct TestProviders {
    ollama: Arc<FakeTransport>,
    cloud: Arc<FakeTransport>,
    creds: Arc<FakeCredentialStore>,
    cloud_built: Arc<Mutex<Vec<String>>>,
}

impl ProviderSource for TestProviders {
    fn local(&self, profile: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        let (text_model, vision_model) = ollama_models(profile)?;
        Ok(Arc::new(OllamaProvider::with_transport(
            OllamaConfig {
                base_url: DEFAULT_OLLAMA_BASE_URL.into(),
                text_model,
                vision_model,
            },
            self.ollama.clone(),
        )?))
    }
    fn cloud(&self, model: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        self.cloud_built.lock().unwrap().push(model.to_string());
        Ok(Arc::new(AnthropicProvider::with_transport(
            AnthropicConfig::new(model),
            self.cloud.clone(),
            self.creds.clone(),
        )?))
    }
}

/// Preview sink that records what the panel was shown, including the served bytes.
#[derive(Default)]
struct PanelSink {
    store: Mutex<Option<Arc<PreviewStore>>>,
    shown: Mutex<Vec<(PreviewDto, Bytes)>>,
    closed: Mutex<Vec<u64>>,
}

impl PreviewSink for PanelSink {
    fn show(&self, dto: PreviewDto) -> bool {
        let body = self
            .store
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.asset(&dto.request_id.to_string(), AssetKind::Body))
            .map(|a| a.bytes)
            .unwrap_or_default();
        self.shown.lock().unwrap().push((dto, body));
        true
    }
    fn close(&self, request_id: u64) {
        self.closed.lock().unwrap().push(request_id);
    }
}

// ------------------------------------------------------------------ tracing capture

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_buf() -> &'static LogBuf {
    static BUF: OnceLock<LogBuf> = OnceLock::new();
    BUF.get_or_init(|| {
        let buf = LogBuf::default();
        let b2 = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || b2.clone())
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
}

fn logs() -> String {
    String::from_utf8_lossy(&log_buf().0.lock().unwrap()).to_string()
}

// ------------------------------------------------------------------ the rig

fn ndjson(texts: &[&str]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = texts
        .iter()
        .map(|t| {
            let mut s =
                serde_json::json!({"message": {"role": "assistant", "content": t}, "done": false})
                    .to_string();
            s.push('\n');
            s.into_bytes()
        })
        .collect();
    v.push(b"{\"done\":true,\"done_reason\":\"stop\"}\n".to_vec());
    v
}

fn sse(texts: &[&str]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = texts
        .iter()
        .map(|t| {
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":t}})
            )
            .into_bytes()
        })
        .collect();
    v.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n".to_vec());
    v.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec());
    v
}

fn node(depth: u16, role: NodeRole, label: Option<&str>, rect: Rect) -> RawTextNode {
    RawTextNode {
        depth,
        role,
        secure: false,
        editable: false,
        label: label.map(String::from),
        value: None,
        has_value: false,
        rect: Some(rect),
    }
}

/// The login page as the accessibility tree sees it. The password node VIOLATES the source
/// contract on purpose (marker as label and value): the firewall must not rely on the source.
fn login_tree(app_id: &str, with_static: Option<&str>) -> RawWindowTree {
    let win = Rect::new(0, 0, 800, 600);
    let mut nodes = vec![
        RawTextNode {
            label: Some("TITLE".into()),
            ..node(0, NodeRole::Window, None, win)
        },
        RawTextNode {
            editable: true,
            ..node(1, NodeRole::TextField, Some("Username"), USERNAME_FIELD)
        },
        RawTextNode {
            secure: true,
            editable: true,
            value: Some(MARKER.into()),
            has_value: true,
            ..node(1, NodeRole::TextField, Some(MARKER), PASSWORD_FIELD)
        },
        node(1, NodeRole::Button, Some("Sign in"), BUTTON),
        node(
            1,
            NodeRole::Link,
            Some("Forgot password?"),
            Rect::new(300, 410, 200, 20),
        ),
    ];
    if let Some(v) = with_static {
        nodes.push(RawTextNode {
            value: Some(v.into()),
            ..node(1, NodeRole::StaticText, None, Rect::new(300, 200, 200, 20))
        });
    }
    RawWindowTree {
        app_id: app_id.into(),
        window_rect: win,
        nodes,
    }
}

/// A nearly empty window: too thin to rely on (route -> insufficient).
fn sparse_tree() -> RawWindowTree {
    let win = Rect::new(0, 0, 800, 600);
    RawWindowTree {
        app_id: "com.example.browser".into(),
        window_rect: win,
        nodes: vec![
            node(0, NodeRole::Window, None, win),
            node(1, NodeRole::Button, Some("Sign in"), BUTTON),
        ],
    }
}

fn speech() -> Vec<AudioChunk> {
    let tone: Vec<f32> = (0..512 * 8).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    let mk = |s: Vec<f32>| AudioChunk {
        samples: s,
        sample_rate: 16_000,
    };
    vec![mk(vec![0.0; 512 * 3]), mk(tone), mk(vec![0.0; 512 * 20])]
}

struct Opts {
    transcript: String,
    reply: Vec<&'static str>,
    tree: Result<RawWindowTree, FirewallError>,
    detector: FakeDetector,
    probe: FakeSessionProbe,
    firewall: Option<Arc<dyn Firewall>>,
    capture_fail: Option<CaptureError>,
    audio: Vec<AudioChunk>,
    endless_mic: bool,
    mic: Option<Arc<dyn AudioInput>>,
    stall_llm: bool,
    stt: Option<Arc<dyn SttProvider>>,
    patch: SettingsPatch,
    key: bool,
    cloud_reply: Vec<&'static str>,
    out: Option<Arc<dyn AudioOutput>>,
    llm_status: u16,
    fake: Option<FakeCapture>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            transcript: QUESTION.into(),
            reply: vec!["It signs you in. ", "[POINT:#3:Sign in]"],
            tree: Ok(login_tree("com.example.browser", None)),
            detector: FakeDetector::new(vec![PASSWORD_FIELD]),
            probe: FakeSessionProbe::normal(),
            firewall: None,
            capture_fail: None,
            audio: speech(),
            endless_mic: false,
            mic: None,
            stall_llm: false,
            stt: None,
            patch: SettingsPatch::default(),
            key: true,
            cloud_reply: vec!["Cloud says hi."],
            out: None,
            llm_status: 200,
            fake: None,
        }
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    buddy: Arc<Buddy>,
    rec: Arc<Rec>,
    captures: Arc<AtomicUsize>,
    ollama: Arc<FakeTransport>,
    cloud: Arc<FakeTransport>,
    cloud_built: Arc<Mutex<Vec<String>>>,
    tts_inputs: Arc<Mutex<Vec<String>>>,
    audio_out: Arc<FakeAudioOut>,
    opens: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    panel: Arc<PanelSink>,
}

fn rig(o: Opts) -> Rig {
    let _ = log_buf();
    let dir = tempfile::tempdir().unwrap();
    let creds = Arc::new(if o.key {
        FakeCredentialStore::with_key(ANTHROPIC_ACCOUNT, "k-test")
    } else {
        FakeCredentialStore::new()
    });
    let captures = Arc::new(AtomicUsize::new(0));
    let mut fake = o
        .fake
        .unwrap_or_else(|| FakeCapture::new(FakeMode::ExcludeWindows));
    fake.fail_with = o.capture_fail.clone();
    let buddy = Arc::new(Buddy::with_credentials(
        dir.path().join("settings.json"),
        Box::new(CountingCapture {
            inner: fake,
            captures: captures.clone(),
        }),
        creds.clone(),
    ));
    buddy.update_settings(&o.patch).unwrap();
    let rec = Arc::new(Rec::default());
    buddy.set_event_sink(rec.clone());
    let panel = Arc::new(PanelSink::default());
    *panel.store.lock().unwrap() = Some(buddy.preview_store());
    buddy.set_preview_sink(panel.clone());

    let source = match o.tree {
        Ok(t) => FakeTextSource::new(t),
        Err(e) => FakeTextSource::failing(e),
    };
    let firewall: Arc<dyn Firewall> = o.firewall.unwrap_or_else(|| {
        Arc::new(PrivacyFirewall::new(o.detector, o.probe).with_text_source(source))
    });

    let chunks = if o.stall_llm {
        FakeTransport::stalling_body(ndjson(&o.reply)[..o.reply.len()].to_vec())
    } else {
        FakeTransport::responding(o.llm_status, ndjson(&o.reply))
    };
    let ollama = Arc::new(chunks);
    let cloud = Arc::new(FakeTransport::responding(200, sse(&o.cloud_reply)));
    let cloud_built = Arc::new(Mutex::new(Vec::new()));

    let tts_inputs = Arc::new(Mutex::new(Vec::new()));
    let audio_out = Arc::new(FakeAudioOut::new());
    let opens = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let out: Arc<dyn AudioOutput> = o.out.unwrap_or_else(|| audio_out.clone());
    let stt: Arc<dyn SttProvider> = o
        .stt
        .unwrap_or_else(|| Arc::new(FakeStt::new(o.transcript)));

    buddy.attach_engine(EngineDeps {
        firewall,
        deny: Arc::new(DenyList::defaults()),
        audio_in: o.mic.unwrap_or_else(|| {
            Arc::new(CountMic {
                inner: FakeAudioIn::new(o.audio),
                opens: opens.clone(),
                live: live.clone(),
                endless: o.endless_mic,
            })
        }),
        audio_out: out,
        vad: Arc::new(|| Ok(Box::new(FakeVad::new()) as Box<dyn Vad>)),
        stt,
        tts: Arc::new(RecTts {
            inner: FakeTts,
            inputs: tts_inputs.clone(),
        }),
        providers: Arc::new(TestProviders {
            ollama: ollama.clone(),
            cloud: cloud.clone(),
            creds,
            cloud_built: cloud_built.clone(),
        }),
        runtime: tokio::runtime::Handle::current(),
    });
    Rig {
        _dir: dir,
        buddy,
        rec,
        captures,
        ollama,
        cloud,
        cloud_built,
        tts_inputs,
        audio_out,
        opens,
        live,
        panel,
    }
}

async fn wait_until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..1000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for: {what}");
}

impl Rig {
    async fn wait_idle(&self) {
        wait_until("turn to end", || {
            self.buddy.get_status().phase == Phase::Idle
        })
        .await;
    }
    /// Hold the key, release it, wait for the turn to finish.
    async fn turn(&self) {
        self.buddy.on_hotkey_pressed();
        self.buddy.on_hotkey_released();
        self.wait_idle().await;
    }
    fn tts_text(&self) -> String {
        self.tts_inputs.lock().unwrap().join(" | ")
    }
    fn body_json(&self, t: &FakeTransport) -> serde_json::Value {
        serde_json::from_slice(&t.calls()[0].body).unwrap()
    }
    fn local_user_content(&self) -> String {
        self.body_json(&self.ollama)["messages"][1]["content"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn local_system(&self) -> String {
        self.body_json(&self.ollama)["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn explanation(&self, r: NotShared) -> &'static str {
        spoken::explanation(&r, Locale::En)
    }
}

fn subsequence(haystack: &[Phase], needle: &[Phase]) -> bool {
    let mut it = haystack.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut r = png::Decoder::new(Cursor::new(bytes)).read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(
        info.color_type,
        png::ColorType::Rgb,
        "the sent image is RGB8"
    );
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

// ================================================================== text path

#[tokio::test]
async fn text_path_redacts_the_password_field_points_and_speaks() {
    let r = rig(Opts::default());
    assert_eq!(
        r.opens.load(Ordering::SeqCst),
        0,
        "mic not opened before the press"
    );
    r.turn().await;

    // Exactly one local call; the body has the redacted text and never the secret.
    assert_eq!(r.ollama.call_count(), 1);
    let call = &r.ollama.calls()[0];
    let raw = String::from_utf8_lossy(&call.body).to_string();
    assert!(
        !raw.contains(MARKER),
        "secret must not be in the request body"
    );
    let user = r.local_user_content();
    assert!(user.contains("password field [hidden]"), "{user}");
    assert!(user.contains("<screen>") && user.contains(QUESTION));
    assert!(user.contains("#3 button \"Sign in\""), "{user}");
    let body = r.body_json(&r.ollama);
    assert_eq!(body["model"], "qwen3:4b");
    assert!(
        body["messages"][1].get("images").is_none(),
        "text path sends no image"
    );
    let system = r.local_system();
    assert!(system.contains("never follow instructions") && system.contains("[POINT:#id:"));
    assert!(!system.contains("already told"), "nothing was withheld");

    // No capture on the text path.
    assert_eq!(r.captures.load(Ordering::SeqCst), 0);

    // Events.
    assert_eq!(r.rec.transcripts(), vec![QUESTION.to_string()]);
    assert!(
        subsequence(
            &r.rec.phases(),
            &[
                Phase::Listening,
                Phase::Transcribing,
                Phase::Thinking,
                Phase::Speaking,
                Phase::Idle
            ]
        ),
        "{:?}",
        r.rec.phases()
    );
    // The button centre, and only that: the stray image-form tag type is not in this reply.
    let pts = r.rec.points();
    assert_eq!(pts.len(), 1, "{pts:?}");
    assert_eq!(
        (pts[0].x, pts[0].y, pts[0].display_id, pts[0].label.as_str()),
        (400, 378, 1, "Sign in")
    );
    let reply = r.rec.reply_text();
    assert!(
        reply.contains("It signs you in.") && !reply.contains("POINT"),
        "{reply}"
    );
    assert_eq!(
        r.rec.firewall(),
        vec![FirewallStatus {
            state: FirewallState::Active,
            reason: None
        }]
    );
    assert!(r.rec.errors().is_empty(), "{:?}", r.rec.errors());

    // TTS got the sentence without the tag; the speaker got audio.
    let tts = r.tts_text();
    assert!(tts.contains("It signs you in."), "{tts}");
    assert!(
        !tts.contains("POINT") && !tts.contains("#3") && !tts.contains(']'),
        "{tts}"
    );
    assert!(!r.audio_out.recorded().is_empty());

    // Last request: redacted, local, no image.
    let last = r.buddy.get_last_request().expect("last request recorded");
    let st = last.screen_text.unwrap();
    assert!(st.contains("password field [hidden]") && !st.contains(MARKER));
    assert_eq!(last.locality, LocalityDto::Local);
    assert!(last.image_url.is_none());
    assert_eq!(last.secure_fields_hidden, 1);
    assert_eq!(last.transcript, QUESTION);
    // Push-to-talk: the stream was opened once, after the press, and is closed.
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert_eq!(r.live.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn image_form_tag_is_dropped_in_text_mode() {
    let r = rig(Opts {
        reply: vec!["Done. [POINT:10,10:nope] [POINT:#9:nowhere]"],
        ..Opts::default()
    });
    r.turn().await;
    assert!(r.rec.points().is_empty(), "{:?}", r.rec.points());
    assert!(!r.tts_text().contains("POINT"));
}

// ================================================================== vision path

#[tokio::test]
async fn vision_always_sends_a_blacked_out_image_and_maps_points() {
    let r = rig(Opts {
        patch: SettingsPatch {
            vision_mode: Some(VisionMode::Always),
            ..Default::default()
        },
        reply: vec!["That button signs you in. [POINT:400,378:Sign in]"],
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 1);
    assert_eq!(r.ollama.call_count(), 1);
    let body = r.body_json(&r.ollama);
    assert_eq!(body["model"], "qwen3-vl:2b");
    let b64 = body["messages"][1]["images"][0]
        .as_str()
        .expect("image in the request");
    let user = r.local_user_content();
    assert!(
        user.contains("password field [hidden]"),
        "text rides along: {user}"
    );
    assert!(!String::from_utf8_lossy(&r.ollama.calls()[0].body).contains(MARKER));
    assert!(r.local_system().contains("800 by 600 pixels"));

    // The previewed PNG is the sent PNG.
    use base64::Engine;
    let png = r
        .buddy
        .preview_store()
        .image_arc("last")
        .expect("last image");
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(&png[..]),
        b64
    );

    // Pixels of the password field are black; the rest of the fixture is intact.
    let (w, h, px) = decode_png(&png);
    let (sx, sy) = (w as f32 / 800.0, h as f32 / 600.0);
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 3) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    let f = PASSWORD_FIELD;
    for y in (f.y as f32 * sy) as u32..((f.y + f.h as i32) as f32 * sy) as u32 {
        for x in (f.x as f32 * sx) as u32..((f.x + f.w as i32) as f32 * sx) as u32 {
            let p = at(x, y);
            assert_eq!(p, [0, 0, 0], "password pixel ({x},{y}) not black");
            assert_ne!(p, [PASSWORD_COLOR[0], PASSWORD_COLOR[1], PASSWORD_COLOR[2]]);
        }
    }
    let u = USERNAME_FIELD;
    let (ux, uy) = (
        ((u.x + 5) as f32 * sx) as u32,
        ((u.y + 5) as f32 * sy) as u32,
    );
    assert_eq!(at(ux, uy), [FIELD_COLOR[0], FIELD_COLOR[1], FIELD_COLOR[2]]);
    assert_eq!(at(5, 5), [255, 255, 255]);

    // Image-form tag mapped through the geometry to global points.
    let pts = r.rec.points();
    assert_eq!(pts.len(), 1, "{pts:?}");
    assert_eq!((pts[0].x, pts[0].y, pts[0].display_id), (400, 378, 1));
    assert!(r.rec.errors().is_empty());
    // The previewed bytes are the sent bytes.
    assert_eq!(
        r.buddy.get_last_request().unwrap().digest_hex.len(),
        64,
        "digest recorded"
    );
}

#[tokio::test]
async fn auto_on_sparse_text_tries_vision_but_low_memory_and_off_never_do() {
    // default profile + Auto + sparse text -> image.
    let r = rig(Opts {
        tree: Ok(sparse_tree()),
        detector: FakeDetector::new(vec![]),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 1);
    assert!(r.body_json(&r.ollama)["messages"][1]["images"].is_array());

    // low-memory forces Off even when the setting says Always.
    let r = rig(Opts {
        tree: Ok(sparse_tree()),
        detector: FakeDetector::new(vec![]),
        patch: SettingsPatch {
            profile: Some("low-memory".into()),
            vision_mode: Some(VisionMode::Always),
            ..Default::default()
        },
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 0);
    let body = r.body_json(&r.ollama);
    assert_eq!(body["model"], "qwen3:1.7b");
    assert!(body["messages"][1].get("images").is_none());
    assert!(
        r.local_system().contains("may be incomplete"),
        "thin text + no vision is announced to the model"
    );

    // vision Off never vision.
    let r = rig(Opts {
        tree: Ok(sparse_tree()),
        detector: FakeDetector::new(vec![]),
        patch: SettingsPatch {
            vision_mode: Some(VisionMode::Off),
            ..Default::default()
        },
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 0);
    assert!(r.body_json(&r.ollama)["messages"][1]
        .get("images")
        .is_none());

    // Auto with good text never needs the screen.
    let r = rig(Opts::default());
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn vision_failures_keep_the_text_and_never_send_an_image() {
    let always = || SettingsPatch {
        vision_mode: Some(VisionMode::Always),
        ..Default::default()
    };
    let shot_msg = spoken::explanation(&NotShared::Screenshot, Locale::En);

    // Capture error -> text only.
    let r = rig(Opts {
        patch: always(),
        capture_fail: Some(CaptureError::PermissionDenied),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 1);
    assert!(r.body_json(&r.ollama)["messages"][1]
        .get("images")
        .is_none());
    assert!(r.local_user_content().contains("password field [hidden]"));
    assert!(r.tts_text().contains(shot_msg), "{}", r.tts_text());
    assert!(r
        .rec
        .firewall()
        .iter()
        .any(|f| f.reason.as_deref() == Some("screenshot_unavailable")));
    assert_eq!(r.ollama.call_count(), 1, "the turn goes on");

    // Detector error during check() after TryVision: the frame is Blocked -> text only.
    // Detector calls: preflight (1), preflight again (2), check (3).
    let r = rig(Opts {
        patch: always(),
        detector: FakeDetector::sequence(vec![
            Ok(vec![PASSWORD_FIELD]),
            Ok(vec![PASSWORD_FIELD]),
            Err(FirewallError::Detection),
        ]),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(
        r.captures.load(Ordering::SeqCst),
        1,
        "capture was attempted"
    );
    assert!(r.body_json(&r.ollama)["messages"][1]
        .get("images")
        .is_none());
    assert!(r.local_user_content().contains("password field [hidden]"));
    assert!(r.tts_text().contains(shot_msg));
    assert!(r.buddy.preview_store().image_arc("last").is_none());

    // The fresh preflight goes Paused (e.g. the screen locked): no capture at all.
    let r = rig(Opts {
        patch: always(),
        probe: FakeSessionProbe::sequence(vec![
            Ok(SessionProbeState::Normal),
            Ok(SessionProbeState::Normal),
            Ok(SessionProbeState::LockScreen),
        ]),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.captures.load(Ordering::SeqCst), 0);
    assert!(r.body_json(&r.ollama)["messages"][1]
        .get("images")
        .is_none());
    assert!(r.tts_text().contains(shot_msg));
}

#[tokio::test]
async fn windows_deny_list_fallback_captures_the_foreground_window_and_still_goes_through_check() {
    // A deny-listed app is visible: the display capture is refused, the (not deny-listed)
    // foreground window is captured instead and passes through the firewall like any frame.
    // The window sits at a NON-ZERO origin so a missing origin override would be detected.
    const FG_WINDOW: Rect = Rect::new(200, 100, 600, 500);
    let fake = FakeCapture::new(FakeMode::WindowsPolicy)
        .with_window("keepassxc.exe", Rect::new(650, 0, 100, 100), [1, 2, 3, 255])
        .with_window("com.example.browser", FG_WINDOW, [9, 9, 9, 255])
        .with_foreground(1);
    let r = rig(Opts {
        fake: Some(fake),
        patch: SettingsPatch {
            vision_mode: Some(VisionMode::Always),
            ..Default::default()
        },
        detector: FakeDetector::new(vec![]),
        tree: Ok(RawWindowTree {
            app_id: "com.example.browser".into(),
            window_rect: FG_WINDOW,
            nodes: vec![
                node(0, NodeRole::Window, None, FG_WINDOW),
                node(1, NodeRole::Button, Some("Sign in"), BUTTON),
            ],
        }),
        reply: vec!["Click it. [POINT:100,50:Sign in]"],
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(
        r.captures.load(Ordering::SeqCst),
        2,
        "display refused, then the foreground window"
    );
    let body = r.body_json(&r.ollama);
    assert!(
        body["messages"][1]["images"].is_array(),
        "{:?}",
        r.rec.firewall()
    );
    let (w, h, _) = decode_png(&r.buddy.preview_store().image_arc("last").unwrap());
    assert_eq!((w, h), (600, 500), "only the window, not the display");
    // Window-origin geometry: the image pixel (100,50) is global (300,150) for a window at
    // (200,100).
    let pts = r.rec.points();
    assert_eq!(pts.len(), 1, "{pts:?}");
    assert_eq!((pts[0].x, pts[0].y), (300, 150));
}

// ================================================================== voice only

#[tokio::test]
async fn blocked_paused_withheld_and_unsupported_screens_are_voice_only() {
    struct Case {
        name: &'static str,
        opts: Opts,
        reason: NotShared,
        state: FirewallState,
        token: &'static str,
    }
    let mut cases = vec![
        Case {
            name: "detector error",
            opts: Opts {
                detector: FakeDetector::failing(FirewallError::Detection),
                ..Opts::default()
            },
            reason: NotShared::Blocked(firewall::BlockReason::DetectionError(
                "secure-field detection failed".into(),
            )),
            state: FirewallState::Blocked,
            token: "detection_error",
        },
        Case {
            name: "lock screen",
            opts: Opts {
                probe: FakeSessionProbe::new(SessionProbeState::LockScreen),
                ..Opts::default()
            },
            reason: NotShared::Paused(PauseReason::LockScreen),
            state: FirewallState::Paused,
            token: "lock_screen",
        },
        Case {
            name: "secure input",
            opts: Opts {
                probe: FakeSessionProbe::new(SessionProbeState::SecureInputActive),
                ..Opts::default()
            },
            reason: NotShared::Paused(PauseReason::SecureInputActive),
            state: FirewallState::Paused,
            token: "secure_input",
        },
        Case {
            name: "denied app",
            opts: Opts {
                tree: Ok(login_tree("com.1password.1password", None)),
                ..Opts::default()
            },
            reason: NotShared::DeniedApp,
            state: FirewallState::Blocked,
            token: "denied_app",
        },
        Case {
            name: "unreadable tree",
            opts: Opts {
                tree: Err(FirewallError::IncompleteTree {
                    app: "com.example.browser".into(),
                }),
                ..Opts::default()
            },
            reason: NotShared::Blocked(firewall::BlockReason::IncompleteTree {
                app: String::new(),
            }),
            state: FirewallState::Blocked,
            token: "incomplete_tree",
        },
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    cases.push(Case {
        name: "unsupported platform",
        opts: Opts {
            firewall: Some(Arc::from(firewall::platform_firewall())),
            ..Opts::default()
        },
        reason: NotShared::Blocked(firewall::BlockReason::Unsupported),
        state: FirewallState::Blocked,
        token: "unsupported",
    });

    for c in cases {
        // Even with vision Always: nothing is captured when the text path is not clear.
        let mut opts = c.opts;
        opts.patch.vision_mode = Some(VisionMode::Always);
        let r = rig(opts);
        r.turn().await;
        assert_eq!(
            r.captures.load(Ordering::SeqCst),
            0,
            "{}: no capture",
            c.name
        );
        assert_eq!(
            r.ollama.call_count(),
            1,
            "{}: the turn goes on voice-only",
            c.name
        );
        let body = r.body_json(&r.ollama);
        assert!(body["messages"][1].get("images").is_none(), "{}", c.name);
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert_eq!(
            user, QUESTION,
            "{}: nothing but the question is sent",
            c.name
        );
        let raw = String::from_utf8_lossy(&r.ollama.calls()[0].body).to_string();
        assert!(
            !raw.contains(MARKER) && !raw.contains("<screen>"),
            "{}",
            c.name
        );
        let system = r.local_system();
        assert!(
            system.contains("cannot see the user's screen") && system.contains("already told"),
            "{}",
            c.name
        );
        assert!(
            !system.contains("[POINT:#id") && !system.contains("[POINT:x,y"),
            "{}",
            c.name
        );
        // The fixed explanation was SPOKEN (first thing TTS received), not model-generated.
        let first = r.tts_inputs.lock().unwrap()[0].clone();
        assert_eq!(first, r.explanation(c.reason.clone()), "{}", c.name);
        // And shown as firewall status.
        let fw = r.rec.firewall();
        assert_eq!(
            fw.last().unwrap(),
            &FirewallStatus {
                state: c.state,
                reason: Some(c.token.into())
            },
            "{}",
            c.name
        );
        assert!(
            r.rec.points().is_empty(),
            "{}: tags are dropped without a screen",
            c.name
        );
        assert!(!r.audio_out.recorded().is_empty(), "{}", c.name);
        // The last request shows nothing of the screen.
        let last = r.buddy.get_last_request().unwrap();
        assert!(
            last.screen_text.is_none() && last.image_url.is_none(),
            "{}",
            c.name
        );
    }
}

// ================================================================== cloud

fn cloud_patch(local_only: bool) -> SettingsPatch {
    SettingsPatch {
        provider: Some(ProviderChoice::Anthropic),
        local_only: Some(local_only),
        anthropic_model: Some("test-model".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn cloud_selected_while_local_only_makes_no_call_and_says_so() {
    let r = rig(Opts {
        patch: cloud_patch(true),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.cloud.call_count(), 0);
    assert_eq!(r.ollama.call_count(), 0);
    assert!(
        r.cloud_built.lock().unwrap().is_empty(),
        "no cloud provider is even built"
    );
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::LocalOnly, Locale::En)
    );
    assert_eq!(r.rec.errors()[0].code, "local_only");
    assert_eq!(r.buddy.get_status().phase, Phase::Idle);
}

#[tokio::test]
async fn cloud_needs_a_model_id_and_a_key_before_a_provider_is_built() {
    // No model id.
    let r = rig(Opts {
        patch: SettingsPatch {
            provider: Some(ProviderChoice::Anthropic),
            local_only: Some(false),
            ..Default::default()
        },
        ..Opts::default()
    });
    r.turn().await;
    assert!(r.cloud_built.lock().unwrap().is_empty());
    assert_eq!(r.cloud.call_count(), 0);
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::CloudNotConfigured, Locale::En)
    );
    // No key.
    let r = rig(Opts {
        patch: cloud_patch(false),
        key: false,
        ..Opts::default()
    });
    r.turn().await;
    assert!(r.cloud_built.lock().unwrap().is_empty());
    assert_eq!(r.cloud.call_count(), 0);
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::NoApiKey, Locale::En)
    );
}

#[tokio::test]
async fn cloud_send_makes_exactly_one_call_with_the_previewed_bytes() {
    let r = rig(Opts {
        patch: cloud_patch(false),
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_released();
    wait_until("preview shown", || {
        !r.panel.shown.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        r.cloud.call_count(),
        0,
        "nothing is sent before the user decides"
    );
    let (dto, shown_body) = r.panel.shown.lock().unwrap()[0].clone();
    assert_eq!(dto.locality, LocalityDto::Cloud);
    assert!(dto
        .screen_text
        .as_ref()
        .unwrap()
        .contains("password field [hidden]"));
    assert!(!String::from_utf8_lossy(&shown_body).contains(MARKER));
    assert_eq!(
        r.buddy.pending_preview().map(|p| p.request_id),
        Some(dto.request_id)
    );
    r.buddy
        .preview_decision(dto.request_id, Decision::Send)
        .unwrap();
    r.wait_idle().await;
    assert_eq!(r.cloud.call_count(), 1);
    assert_eq!(r.ollama.call_count(), 0);
    assert_eq!(
        r.cloud.calls()[0].body,
        shown_body,
        "sent bytes == previewed bytes"
    );
    assert_eq!(
        *r.cloud_built.lock().unwrap(),
        vec!["test-model".to_string()]
    );
    assert!(r.tts_text().contains("Cloud says hi."));
    assert!(r.buddy.pending_preview().is_none());
}

#[tokio::test]
async fn cloud_cancel_sends_nothing() {
    let r = rig(Opts {
        patch: cloud_patch(false),
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_released();
    wait_until("preview shown", || {
        !r.panel.shown.lock().unwrap().is_empty()
    })
    .await;
    let id = r.panel.shown.lock().unwrap()[0].0.request_id;
    r.buddy.preview_decision(id, Decision::Cancel).unwrap();
    r.wait_idle().await;
    assert_eq!(r.cloud.call_count(), 0);
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::Cancelled, Locale::En)
    );
}

#[tokio::test(start_paused = true)]
async fn cloud_preview_timeout_sends_nothing() {
    let r = rig(Opts {
        patch: cloud_patch(false),
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_released();
    wait_until("preview shown", || {
        !r.panel.shown.lock().unwrap().is_empty()
    })
    .await;
    tokio::time::advance(Duration::from_secs(121)).await;
    r.wait_idle().await;
    assert_eq!(r.cloud.call_count(), 0);
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::Cancelled, Locale::En)
    );
}

#[tokio::test]
async fn a_hotkey_press_during_a_pending_preview_cancels_it() {
    let r = rig(Opts {
        patch: cloud_patch(false),
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_released();
    wait_until("preview shown", || {
        !r.panel.shown.lock().unwrap().is_empty()
    })
    .await;
    let id = r.panel.shown.lock().unwrap()[0].0.request_id;
    assert!(r.buddy.pending_preview().is_some());
    // Press again: the old turn is cancelled, its preview closed, a new turn is listening.
    r.buddy.on_hotkey_pressed();
    assert!(r.buddy.pending_preview().is_none());
    assert!(r.panel.closed.lock().unwrap().contains(&id));
    assert_eq!(r.buddy.get_status().phase, Phase::Listening);
    assert_eq!(
        r.buddy.preview_decision(id, Decision::Send),
        Err(buddy_core::consent::ConsentError::UnknownRequest)
    );
    r.buddy.cancel_turn();
    r.wait_idle().await;
    assert_eq!(r.cloud.call_count(), 0);
}

// ================================================================== cancel, push-to-talk, quiet ends

#[tokio::test]
async fn a_press_during_a_turn_drops_the_stream_stops_playback_and_starts_over() {
    let stopped = Arc::new(AtomicBool::new(false));
    let plays = Arc::new(AtomicUsize::new(0));
    let r = rig(Opts {
        stall_llm: true,
        reply: vec!["Hello there. "],
        out: Some(Arc::new(StopOut {
            stopped: stopped.clone(),
            plays: plays.clone(),
        })),
        ..Opts::default()
    });
    let body_dropped = r.ollama.body_dropped_flag();
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_released();
    wait_until("speaking", || r.buddy.get_status().phase == Phase::Speaking).await;
    wait_until("playback started", || plays.load(Ordering::SeqCst) == 1).await;
    assert!(!stopped.load(Ordering::SeqCst) && !body_dropped.load(Ordering::SeqCst));

    r.buddy.on_hotkey_pressed(); // barge in
    wait_until("stream dropped", || body_dropped.load(Ordering::SeqCst)).await;
    wait_until("playback stopped", || stopped.load(Ordering::SeqCst)).await;
    let st = r.buddy.get_status();
    assert_eq!(st.phase, Phase::Listening);
    let phases = r.rec.phases();
    assert!(
        subsequence(&phases, &[Phase::Speaking, Phase::Idle, Phase::Listening]),
        "{phases:?}"
    );
    r.buddy.cancel_turn();
    assert_eq!(r.buddy.get_status().phase, Phase::Idle);
}

#[tokio::test]
async fn push_to_talk_opens_after_the_press_and_closes_on_release_or_cancel() {
    // Held key: an endless stream that stays open until release.
    let r = rig(Opts {
        endless_mic: true,
        ..Opts::default()
    });
    assert_eq!(r.opens.load(Ordering::SeqCst), 0);
    r.buddy.on_hotkey_pressed();
    wait_until("mic open", || r.live.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(r.live.load(Ordering::SeqCst), 1, "still held");
    assert_eq!(r.buddy.get_status().phase, Phase::Listening);
    r.buddy.on_hotkey_released();
    wait_until("mic closed after release", || {
        r.live.load(Ordering::SeqCst) == 0
    })
    .await;
    r.wait_idle().await;
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert_eq!(r.ollama.call_count(), 1);

    // Cancel while held closes the mic too, and no STT/LLM runs.
    let r = rig(Opts {
        endless_mic: true,
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    wait_until("mic open", || r.live.load(Ordering::SeqCst) == 1).await;
    r.buddy.cancel_turn();
    wait_until("mic closed after cancel", || {
        r.live.load(Ordering::SeqCst) == 0
    })
    .await;
    assert_eq!(r.buddy.get_status().phase, Phase::Idle);
    assert_eq!(r.ollama.call_count(), 0);

    // Auto-repeat of the held key does not start a second turn.
    let r = rig(Opts {
        endless_mic: true,
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_pressed();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    r.buddy.cancel_turn();
}

#[tokio::test(start_paused = true)]
async fn the_recording_cap_finishes_the_turn_like_a_release_and_a_later_press_starts_a_new_one() {
    // A mic that never ends and never gets a release: after 60 s of audio the cap closes it
    // and the turn moves on exactly as on a release.
    struct Firehose(Arc<AtomicUsize>);
    impl AudioInput for Firehose {
        fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut first = true;
            Ok(stream::unfold((), move |()| {
                let speak = std::mem::take(&mut first);
                async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let samples = if speak {
                        (0..16_000).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect()
                    } else {
                        vec![0.0; 16_000]
                    };
                    Some((
                        AudioChunk {
                            samples,
                            sample_rate: 16_000,
                        },
                        (),
                    ))
                }
            })
            .boxed())
        }
    }
    let opens = Arc::new(AtomicUsize::new(0));
    let r = rig(Opts {
        mic: Some(Arc::new(Firehose(opens.clone()))),
        ..Opts::default()
    });
    r.buddy.on_hotkey_pressed();
    r.buddy.on_hotkey_pressed(); // auto-repeat while listening: ignored
    assert_eq!(r.buddy.get_status().phase, Phase::Listening);
    // 30 s of audio in: still listening.
    for _ in 0..30 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(r.buddy.get_status().phase, Phase::Listening);
    // No Released event ever arrives: the 60 s cap ends the recording.
    for _ in 0..40 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    r.wait_idle().await;
    let phases = r.rec.phases();
    assert!(
        subsequence(
            &phases,
            &[Phase::Listening, Phase::Transcribing, Phase::Idle]
        ),
        "{phases:?}"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    // A later press starts a new turn (no stuck state).
    r.buddy.on_hotkey_pressed();
    assert_eq!(r.buddy.get_status().phase, Phase::Listening);
    wait_until("second mic open", || opens.load(Ordering::SeqCst) == 2).await;
    r.buddy.cancel_turn();
}

#[tokio::test]
async fn silence_and_empty_transcripts_end_quietly() {
    // No speech at all: STT never runs.
    let r = rig(Opts {
        audio: vec![AudioChunk {
            samples: vec![0.0; 512 * 30],
            sample_rate: 16_000,
        }],
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.ollama.call_count(), 0);
    assert!(r.rec.transcripts().is_empty() && r.rec.errors().is_empty());
    assert!(r.tts_inputs.lock().unwrap().is_empty(), "nothing is spoken");
    assert_eq!(r.rec.phases().last(), Some(&Phase::Idle));

    // Speech but an empty transcript.
    let r = rig(Opts {
        transcript: "   ".into(),
        ..Opts::default()
    });
    r.turn().await;
    assert_eq!(r.ollama.call_count(), 0);
    assert!(r.rec.transcripts().is_empty() && r.rec.errors().is_empty());
    assert_eq!(r.rec.phases().last(), Some(&Phase::Idle));
}

#[tokio::test]
async fn voice_failures_are_reported_with_fixed_tokens() {
    // STT model file missing.
    let r = rig(Opts {
        stt: Some(Arc::new(MissingStt)),
        ..Opts::default()
    });
    r.turn().await;
    let errs = r.rec.errors();
    assert_eq!(errs[0].code, "model_missing");
    assert!(
        errs[0].message.contains("/models/whisper-small.bin"),
        "names the expected file"
    );
    assert_eq!(r.ollama.call_count(), 0);
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::SttFailed, Locale::En)
    );
    assert_eq!(r.buddy.get_status().phase, Phase::Idle);
}

#[tokio::test]
async fn llm_failure_is_a_fixed_error_and_a_spoken_apology() {
    let r = rig(Opts {
        llm_status: 500,
        ..Opts::default()
    });
    r.turn().await;
    let errs = r.rec.errors();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].code, "llm_http");
    assert_eq!(
        r.tts_inputs.lock().unwrap().last().unwrap(),
        spoken::notice(Notice::LlmFailed, Locale::En)
    );
    assert_eq!(r.buddy.get_status().phase, Phase::Idle);
}

// ================================================================== privacy marker

#[tokio::test]
async fn private_text_never_leaks_into_events_errors_or_logs() {
    let said = format!("tell me about {MARKER}");
    let reply: Vec<&'static str> = vec![
        "Sure, ",
        "the thing is PRIVATE_MARKER_8431. ",
        "[POINT:#3:Sign in]",
    ];
    let r = rig(Opts {
        transcript: said.clone(),
        reply,
        tree: Ok(login_tree("com.example.browser", Some(MARKER))),
        ..Opts::default()
    });
    r.turn().await;
    // The marker is INTENDED content only in transcript and reply events (and, by design, in
    // the request, because it was visible static text and the user said it).
    for ev in r.rec.all() {
        match &ev {
            Ev::Transcript(_) | Ev::Reply(_) => {}
            other => assert!(!format!("{other:?}").contains(MARKER), "leak in {other:?}"),
        }
    }
    assert!(r.rec.transcripts()[0].contains(MARKER));
    assert!(
        r.local_user_content().contains(MARKER),
        "visible static text is sent (locally)"
    );
    // The secure node's own text (label and value) is never anywhere.
    // (It is the same marker string, so use the dedicated check in the password test below.)
    assert!(
        logs().contains("turn start"),
        "log capture must be alive for the absence check to mean anything"
    );
    assert!(
        !logs().contains(MARKER),
        "tracing output must not contain content"
    );
    assert!(r.rec.errors().is_empty());

    // Errors carry no content either.
    let r = rig(Opts {
        transcript: said,
        llm_status: 500,
        ..Opts::default()
    });
    r.turn().await;
    for e in r.rec.errors() {
        assert!(!format!("{e:?}").contains(MARKER));
    }
    assert!(logs().contains("turn start"));
    assert!(!logs().contains(MARKER));
}

#[tokio::test]
async fn secure_field_text_is_in_no_request_event_tts_or_log() {
    let r = rig(Opts {
        patch: SettingsPatch {
            vision_mode: Some(VisionMode::Always),
            ..Default::default()
        },
        ..Opts::default()
    });
    r.turn().await;
    assert!(logs().contains("turn start"), "log capture must be alive");
    let everything = format!(
        "{:?} {:?} {} {} {}",
        r.rec.all(),
        r.buddy.get_last_request(),
        String::from_utf8_lossy(&r.ollama.calls()[0].body),
        r.tts_text(),
        logs()
    );
    assert!(
        !everything.contains(MARKER),
        "the secure node's label/value must not appear anywhere"
    );
}
