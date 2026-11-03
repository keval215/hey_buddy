//! The turn engine (ADR 0001 turn flow as amended by ADR 0002 A5-A7, ticket T12).
//!
//! One turn runs on one tokio task, started by [`crate::Buddy::on_hotkey_pressed`]:
//!
//! ```text
//! press   -> Listening: open the push-to-talk mic; IN PARALLEL
//!            preflight -> check_text(deny) -> route -> [fresh preflight -> capture -> check]
//! release -> Transcribing: close the mic, VAD trim, STT (empty transcript = quiet end)
//!         -> Thinking: prompt -> LlmRequest -> Buddy::prepare_and_consent (THE path to a
//!            PreparedRequest and, for cloud, the digest-bound opt-in) -> stream
//!         -> Speaking: deltas -> PointStream (tags -> turn:point) -> SentenceChunker -> TTS ->
//!            AudioOutput; first audio before the reply ends
//!         -> Idle
//! ```
//!
//! Image rule: only a [`Verdict::Clear`] frame ever becomes part of a request. Paused, blocked,
//! failed or timed-out capture/check mean "no image" and the turn goes on (text only, or voice
//! only when the text path was not clear either). The `llm` types already make anything else a
//! compile error; this module keeps it that way by never holding an unredacted frame past
//! `Firewall::check`.
//!
//! Cancelling (a new press, `cancel_turn`) aborts the task: every stream, the mic, the
//! playback handle and a pending consent are dropped. Errors and logs carry fixed tokens only,
//! never a transcript, reply or screen text.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use capture::{CaptureError, CaptureRequest, CaptureScope, DenyList};
use firewall::{
    BlockReason, Firewall, RedactedFrame, RedactedScreenText, SessionState, TextVerdict, Verdict,
};
use frame::{DisplayInfo, Rect};
use futures::channel::mpsc;
use futures::stream::{self, BoxStream, StreamExt};
use futures::FutureExt;
use llm::{LlmDelta, LlmError, LlmProvider, LlmRequest, ScreenContext};
use tokio::sync::watch;
use voice::{
    AudioChunk, AudioInput, AudioOutput, PlaybackHandle, SegmentEvent, Segmenter, SegmenterConfig,
    SentenceChunker, SttEvent, SttOptions, SttProvider, TtsOptions, TtsProvider, Vad, VoiceError,
};

use crate::api::{Phase, ProviderChoice, ReplyDelta, Settings, TranscriptEvent, TurnError};
use crate::buddy::Buddy;
use crate::error::{llm_turn_error, simple_turn_error, voice_turn_error, CoreError};
use crate::point::{self, NodeContext, PointContext, PointStream};
use crate::prompt::{system_prompt, PromptScreen, PromptSpec};
use crate::route::{profile_has_vlm, route, text_is_insufficient, Route};
use crate::settings::builtin_registry;
use crate::spoken::{self, status_active, Locale, NotShared, Notice};

/// Longest recording kept in memory (16 kHz mono samples). Audio past it is dropped.
const MAX_RECORD_SECS: usize = 60;
/// Longest wait for one screen capture.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(8);
/// Windows of audio kept before the detected speech start (the segmenter needs a few windows
/// to confirm a start).
const PRE_ROLL_WINDOWS: usize = 8;
const SAMPLE_RATE: u32 = 16_000;
const LOCALE: Locale = Locale::En;

/// Creates a fresh voice-activity detector for one recording.
pub type VadFactory = Arc<dyn Fn() -> Result<Box<dyn Vad>, VoiceError> + Send + Sync>;

/// Where the engine gets its language-model providers.
pub trait ProviderSource: Send + Sync {
    /// The local (Ollama, loopback) provider for the registry profile `profile`.
    fn local(&self, profile: &str) -> Result<Arc<dyn LlmProvider>, LlmError>;
    /// The cloud provider for `model`. Only called when the user chose it AND `local_only` is
    /// off AND the model id is set AND a key is stored.
    fn cloud(&self, model: &str) -> Result<Arc<dyn LlmProvider>, LlmError>;
}

/// Everything a turn needs besides the settings, capture and consent that [`Buddy`] owns.
/// All trait objects, so the whole engine runs on fakes.
pub struct EngineDeps {
    pub firewall: Arc<dyn Firewall>,
    pub deny: Arc<DenyList>,
    pub audio_in: Arc<dyn AudioInput>,
    pub audio_out: Arc<dyn AudioOutput>,
    pub vad: VadFactory,
    pub stt: Arc<dyn SttProvider>,
    pub tts: Arc<dyn TtsProvider>,
    pub providers: Arc<dyn ProviderSource>,
    /// Runtime the turn tasks run on.
    pub runtime: tokio::runtime::Handle,
}

/// Handle on the running turn, owned by `Buddy`.
pub(crate) struct TurnControl {
    pub turn_id: u32,
    pub cancelled: Arc<AtomicBool>,
    pub release: watch::Sender<bool>,
    pub task: tokio::task::JoinHandle<()>,
}

impl TurnControl {
    /// Stop everything: the task (and so mic, streams, playback, pending consent) is dropped.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

/// What the task knows about itself.
pub(crate) struct TurnCtx {
    pub buddy: Arc<Buddy>,
    pub deps: Arc<EngineDeps>,
    pub turn_id: u32,
    pub cancelled: Arc<AtomicBool>,
    pub release: watch::Receiver<bool>,
}

impl TurnCtx {
    fn alive(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
    }
    fn phase(&self, p: Phase) {
        if self.alive() {
            self.buddy.advance_phase(self.turn_id, p);
        }
    }
    fn error(&self, e: TurnError) {
        if self.alive() {
            tracing::debug!(code = %e.code, "turn error");
            self.buddy.sink().error(e);
        }
    }
    fn voice_error(&self, e: &VoiceError) {
        if let Some(t) = voice_turn_error(e) {
            self.error(t);
        }
    }
    fn firewall(&self, st: crate::api::FirewallStatus) {
        if self.alive() {
            self.buddy.set_firewall_status(st);
        }
    }
}

/// Task entry point.
pub(crate) async fn run(ctx: TurnCtx) {
    tracing::debug!(turn_id = ctx.turn_id, "turn start");
    let res = AssertUnwindSafe(turn(&ctx)).catch_unwind().await;
    if res.is_err() {
        ctx.error(simple_turn_error(
            "internal_error",
            "The turn failed unexpectedly.",
        ));
    }
    ctx.buddy.finish_turn(ctx.turn_id);
    tracing::debug!(turn_id = ctx.turn_id, "turn end");
}

async fn turn(ctx: &TurnCtx) {
    let settings = ctx.buddy.get_settings();
    let vlm = builtin_registry()
        .profile(&settings.profile)
        .is_some_and(profile_has_vlm);

    let (heard, prep) = tokio::join!(
        listen_and_transcribe(ctx),
        prepare_screen(ctx, settings.vision_mode.into(), vlm)
    );
    let mut speaker = Speaker::new(ctx);
    let transcript = match heard {
        Heard::Text(t) => t,
        Heard::Quiet => return,
        Heard::Failed(notice) => {
            if let Some(n) = notice {
                speaker.say(spoken::notice(n, LOCALE)).await;
                speaker.finish().await;
            }
            return;
        }
    };
    if ctx.alive() {
        ctx.buddy.sink().transcript(TranscriptEvent {
            turn_id: ctx.turn_id,
            text: transcript.clone(),
        });
    }
    ctx.phase(Phase::Thinking);
    answer(ctx, &mut speaker, &settings, transcript, prep).await;
    speaker.finish().await;
}

// ------------------------------------------------------------------------- listening

enum Heard {
    Text(String),
    /// Nothing was said (no speech, or an empty transcript): end quietly.
    Quiet,
    /// A voice component failed (error already emitted). The notice is spoken best effort.
    Failed(Option<Notice>),
}

async fn listen_and_transcribe(ctx: &TurnCtx) -> Heard {
    // Push-to-talk: the stream is opened here, after the press, and dropped on release.
    let stream = match ctx.deps.audio_in.open_push_to_talk() {
        Ok(s) => s,
        Err(e) => {
            ctx.voice_error(&e);
            return Heard::Failed(None);
        }
    };
    let samples = record(ctx, stream).await;
    ctx.phase(Phase::Transcribing);

    let make_vad = ctx.deps.vad.clone();
    let trimmed = tokio::task::spawn_blocking(move || {
        let mut vad = make_vad()?;
        trim_to_speech(vad.as_mut(), &samples)
    })
    .await;
    let speech = match trimmed {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            ctx.voice_error(&e);
            return Heard::Failed(Some(Notice::SttFailed));
        }
        Err(_) => {
            ctx.error(simple_turn_error("voice_failed", "A voice engine failed."));
            return Heard::Failed(None);
        }
    };
    if speech.is_empty() {
        return Heard::Quiet;
    }
    let chunks: Vec<AudioChunk> = speech
        .chunks(SAMPLE_RATE as usize)
        .map(|c| AudioChunk {
            samples: c.to_vec(),
            sample_rate: SAMPLE_RATE,
        })
        .collect();
    let mut events = match ctx
        .deps
        .stt
        .transcribe(stream::iter(chunks).boxed(), SttOptions::default())
        .await
    {
        Ok(e) => e,
        Err(e) => {
            ctx.voice_error(&e);
            return Heard::Failed(Some(Notice::SttFailed));
        }
    };
    let mut text = None;
    while let Some(ev) = events.next().await {
        match ev {
            Ok(SttEvent::Final(t)) => text = Some(t.text),
            Ok(SttEvent::Partial(_)) => {}
            Err(e) => {
                ctx.voice_error(&e);
                return Heard::Failed(Some(Notice::SttFailed));
            }
        }
    }
    match text.map(|t| t.trim().to_string()) {
        Some(t) if !t.is_empty() => Heard::Text(t),
        _ => Heard::Quiet,
    }
}

/// Collect mic audio (16 kHz mono) until the key is released, then drop the stream, which
/// closes the microphone.
async fn record(ctx: &TurnCtx, stream: BoxStream<'static, AudioChunk>) -> Vec<f32> {
    let cap = MAX_RECORD_SECS * SAMPLE_RATE as usize;
    let mut rx = ctx.release.clone();
    let mut samples: Vec<f32> = Vec::new();
    let mut stream = Some(stream);
    let push = |samples: &mut Vec<f32>, c: AudioChunk| {
        let c = if c.sample_rate == SAMPLE_RATE {
            c
        } else {
            voice::resample_to_16k_mono(&c, 1)
        };
        let room = cap.saturating_sub(samples.len());
        samples.extend(c.samples.into_iter().take(room));
    };
    loop {
        if *rx.borrow() {
            break;
        }
        tokio::select! {
            biased;
            c = async { stream.as_mut().expect("guarded").next().await }, if stream.is_some() => {
                match c {
                    Some(c) => {
                        push(&mut samples, c);
                        if samples.len() >= cap {
                            // Full: finish exactly as if the key had been released (the
                            // mic closes and the turn moves on), so the engine can never sit
                            // in Listening with auto-repeat presses ignored.
                            break;
                        }
                    }
                    None => stream = None,
                }
            }
            r = rx.changed() => {
                if r.is_err() {
                    break; // the sender is gone: the turn was cancelled
                }
            }
        }
    }
    // Take what the stream already has, then close it.
    if let Some(mut s) = stream.take() {
        while let Some(Some(c)) = s.next().now_or_never() {
            push(&mut samples, c);
        }
    }
    samples
}

/// Keep the audio from the first confirmed speech start (with a short pre-roll) up to the end
/// of the last speech segment. Empty when no speech was detected.
pub(crate) fn trim_to_speech(vad: &mut dyn Vad, samples: &[f32]) -> Result<Vec<f32>, VoiceError> {
    let w = vad.window_len().max(1);
    let mut seg = Segmenter::new(SegmenterConfig::default());
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    let windows = samples.len() / w;
    for i in 0..windows {
        let p = vad.speech_prob(&samples[i * w..(i + 1) * w])?;
        match seg.push(p) {
            Some(SegmentEvent::SpeechStart) => {
                if start.is_none() {
                    start = Some(i.saturating_sub(PRE_ROLL_WINDOWS));
                }
                end = None;
            }
            Some(SegmentEvent::SpeechEnd) => end = Some(i + 1),
            None => {}
        }
    }
    let Some(start) = start else {
        return Ok(Vec::new());
    };
    let end = end.unwrap_or(windows).min(windows);
    Ok(samples[start * w..end * w].to_vec())
}

// ------------------------------------------------------------------------- the screen

/// Outcome of the press-time screen preparation.
pub(crate) struct ScreenPrep {
    text: Option<RedactedScreenText>,
    image: Option<Shot>,
    /// Set whenever the screen (or the screenshot) is not shared.
    not_shared: Option<NotShared>,
}

#[derive(Clone)]
struct Shot {
    frame: RedactedFrame,
    /// The frame is the foreground window only (Windows deny-list fallback), not the display.
    window_fallback: bool,
}

impl ScreenPrep {
    fn voice_only(r: NotShared) -> Self {
        Self {
            text: None,
            image: None,
            not_shared: Some(r),
        }
    }
    fn text_only(text: RedactedScreenText) -> Self {
        Self {
            text: Some(text),
            image: None,
            not_shared: None,
        }
    }
    fn screenshot_failed(text: RedactedScreenText) -> Self {
        Self {
            text: Some(text),
            image: None,
            not_shared: Some(NotShared::Screenshot),
        }
    }
}

/// A firewall call off the async threads. `None` = the task failed (panic): callers fail closed.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

fn internal_block() -> NotShared {
    NotShared::Blocked(BlockReason::DetectionError("firewall task failed".into()))
}

fn contains(r: &Rect, x: i64, y: i64) -> bool {
    !r.is_empty()
        && x >= r.x as i64
        && y >= r.y as i64
        && x < r.x as i64 + r.w as i64
        && y < r.y as i64 + r.h as i64
}

async fn prepare_screen(ctx: &TurnCtx, vision: crate::route::VisionMode, vlm: bool) -> ScreenPrep {
    let fw = ctx.deps.firewall.clone();
    match blocking(move || fw.preflight()).await {
        Some(SessionState::Active) => {}
        Some(SessionState::Paused(r)) => return ScreenPrep::voice_only(NotShared::Paused(r)),
        Some(SessionState::Blocked(r)) => return ScreenPrep::voice_only(NotShared::Blocked(r)),
        None => return ScreenPrep::voice_only(internal_block()),
    }
    let (fw, deny) = (ctx.deps.firewall.clone(), ctx.deps.deny.clone());
    let Some(verdict) = blocking(move || fw.check_text(&*deny)).await else {
        return ScreenPrep::voice_only(internal_block());
    };
    let next = route(&verdict, vision, vlm);
    let text = match verdict {
        TextVerdict::Clear(t) => t,
        TextVerdict::Paused(r) => return ScreenPrep::voice_only(NotShared::Paused(r)),
        TextVerdict::Blocked(r) => return ScreenPrep::voice_only(NotShared::Blocked(r)),
        TextVerdict::Withheld(_) => return ScreenPrep::voice_only(NotShared::DeniedApp),
    };
    match next {
        Route::TextOnly => ScreenPrep::text_only(text),
        Route::TryVision => try_vision(ctx, text).await,
        // route() never does this for a Clear verdict; fail closed anyway.
        Route::VoiceOnly(_) => {
            ScreenPrep::voice_only(NotShared::Blocked(BlockReason::UnstableLayout))
        }
    }
}

/// Fresh preflight -> capture -> check. Only `Verdict::Clear` yields an image; every other
/// outcome keeps the (already clear) text and drops the image.
async fn try_vision(ctx: &TurnCtx, text: RedactedScreenText) -> ScreenPrep {
    let fw = ctx.deps.firewall.clone();
    if !matches!(
        blocking(move || fw.preflight()).await,
        Some(SessionState::Active)
    ) {
        return ScreenPrep::screenshot_failed(text);
    }
    let Ok(displays) = ctx.buddy.capture.displays() else {
        return ScreenPrep::screenshot_failed(text);
    };
    let w = text.window_rect();
    let (cx, cy) = (w.x as i64 + (w.w / 2) as i64, w.y as i64 + (w.h / 2) as i64);
    let Some(display) = displays.iter().find(|d| contains(&d.bounds(), cx, cy)) else {
        return ScreenPrep::screenshot_failed(text);
    };
    let request = |scope| CaptureRequest {
        display: display.id,
        deny_list: (*ctx.deps.deny).clone(),
        scope,
    };
    let capture = |scope| async move {
        tokio::time::timeout(CAPTURE_TIMEOUT, ctx.buddy.capture.capture(&request(scope))).await
    };
    let (frame, window_fallback) = match capture(CaptureScope::Display).await {
        Ok(Ok(f)) => (f, false),
        // A deny-listed app is visible on the display: capture only the foreground window,
        // which the capture source refuses unless that window is itself not deny-listed.
        Ok(Err(CaptureError::DeniedAppVisible { .. })) => {
            match capture(CaptureScope::ForegroundWindow).await {
                Ok(Ok(f)) => (f, true),
                _ => return ScreenPrep::screenshot_failed(text),
            }
        }
        _ => return ScreenPrep::screenshot_failed(text),
    };
    let fw = ctx.deps.firewall.clone();
    match blocking(move || fw.check(frame)).await {
        Some(Verdict::Clear(frame)) => ScreenPrep {
            text: Some(text),
            image: Some(Shot {
                frame,
                window_fallback,
            }),
            not_shared: None,
        },
        _ => ScreenPrep::screenshot_failed(text),
    }
}

// ------------------------------------------------------------------------- speaking

/// Feeds sentences to TTS -> audio output. The TTS stream is started lazily at the first
/// sentence, so silent turns never touch the speaker.
struct Speaker<'a> {
    ctx: &'a TurnCtx,
    tx: Option<mpsc::UnboundedSender<String>>,
    handle: Option<PlaybackHandle>,
    failed: bool,
    said_any: bool,
}

impl<'a> Speaker<'a> {
    fn new(ctx: &'a TurnCtx) -> Self {
        Self {
            ctx,
            tx: None,
            handle: None,
            failed: false,
            said_any: false,
        }
    }

    async fn start(&mut self) {
        let (tx, rx) = mpsc::unbounded::<String>();
        let audio = match self
            .ctx
            .deps
            .tts
            .synthesize(rx.boxed(), TtsOptions::default())
            .await
        {
            Ok(a) => a,
            Err(e) => return self.fail(&e),
        };
        match self.ctx.deps.audio_out.play(audio) {
            Ok(h) => {
                self.tx = Some(tx);
                self.handle = Some(h);
            }
            Err(e) => self.fail(&e),
        }
    }

    fn fail(&mut self, e: &VoiceError) {
        self.failed = true;
        self.ctx.voice_error(e);
    }

    async fn say(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() || self.failed {
            return;
        }
        self.said_any = true;
        self.ctx.phase(Phase::Speaking);
        if self.tx.is_none() {
            self.start().await;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.unbounded_send(text.to_string());
        }
    }

    /// Close the text stream and wait until everything has been played.
    async fn finish(&mut self) {
        self.tx = None;
        if let Some(h) = self.handle.take() {
            h.finished().await;
        }
    }
}

// ------------------------------------------------------------------------- the answer

type Refusal = (Notice, Option<TurnError>);

fn choose_provider(ctx: &TurnCtx, s: &Settings) -> Result<Arc<dyn LlmProvider>, Refusal> {
    match s.provider {
        ProviderChoice::Ollama => ctx
            .deps
            .providers
            .local(&s.profile)
            .map_err(|e| (Notice::LlmFailed, Some(llm_turn_error(&e)))),
        ProviderChoice::Anthropic => {
            if s.local_only {
                return Err((
                    Notice::LocalOnly,
                    Some(simple_turn_error(
                        "local_only",
                        "Local only mode is on: nothing is sent to the cloud.",
                    )),
                ));
            }
            // T11 review follow-up: never build the Anthropic provider without a model id.
            if !s.cloud_configured() {
                return Err((
                    Notice::CloudNotConfigured,
                    Some(simple_turn_error(
                        "cloud_not_configured",
                        "No cloud model name is set.",
                    )),
                ));
            }
            if !s.has_api_key {
                return Err((
                    Notice::NoApiKey,
                    Some(simple_turn_error(
                        "llm_no_credentials",
                        "No API key is stored.",
                    )),
                ));
            }
            ctx.deps
                .providers
                .cloud(s.anthropic_model.trim())
                .map_err(|e| (Notice::LlmFailed, Some(llm_turn_error(&e))))
        }
    }
}

struct Built {
    request: LlmRequest,
    geometry: Option<point::ImageGeometry>,
}

fn build_request(
    text: &Option<RedactedScreenText>,
    image: Option<&Shot>,
    transcript: &str,
    announced: bool,
) -> Result<Built, LlmError> {
    match (text, image) {
        (t, Some(shot)) => {
            let screen = || ScreenContext::TextAndImage {
                text: t.clone(),
                image: shot.frame.clone(),
            };
            // The prompt states the size of the image that is really sent, which is only known
            // after encoding: encode once to learn it, then build the final request.
            let probe = LlmRequest::new(String::new(), String::new(), screen())?;
            let g = probe
                .image_geometry()
                .ok_or(LlmError::Encode("no_geometry"))?;
            drop(probe);
            let system = system_prompt(PromptSpec {
                screen: PromptScreen::Image { sent_px: g.sent_px },
                announced,
                incomplete: false,
            });
            let request = LlmRequest::new(system, transcript.to_string(), screen())?;
            let g = request
                .image_geometry()
                .ok_or(LlmError::Encode("no_geometry"))?;
            let mut origin = g.origin;
            if shot.window_fallback {
                // The frame is the foreground window, not the display: its top-left is the
                // window's top-left.
                if let Some(t) = t {
                    let w = t.window_rect();
                    origin = (w.x, w.y);
                }
            }
            Ok(Built {
                request,
                geometry: Some(point::ImageGeometry {
                    origin,
                    scale: g.scale,
                    frame_px: g.frame_px,
                    sent_px: g.sent_px,
                    display_id: g.display_id,
                }),
            })
        }
        (Some(t), None) => {
            let system = system_prompt(PromptSpec {
                screen: PromptScreen::Text,
                announced,
                incomplete: text_is_insufficient(&t.coverage()),
            });
            Ok(Built {
                request: LlmRequest::new(
                    system,
                    transcript.to_string(),
                    ScreenContext::Text(t.clone()),
                )?,
                geometry: None,
            })
        }
        (None, None) => {
            let system = system_prompt(PromptSpec {
                screen: PromptScreen::None,
                announced,
                incomplete: false,
            });
            Ok(Built {
                request: LlmRequest::new(system, transcript.to_string(), ScreenContext::None)?,
                geometry: None,
            })
        }
    }
}

async fn answer(
    ctx: &TurnCtx,
    sp: &mut Speaker<'_>,
    settings: &Settings,
    transcript: String,
    prep: ScreenPrep,
) {
    let ScreenPrep {
        text,
        image,
        mut not_shared,
    } = prep;
    ctx.firewall(
        not_shared
            .as_ref()
            .map_or_else(status_active, NotShared::status),
    );

    let provider = match choose_provider(ctx, settings) {
        Ok(p) => p,
        Err((n, err)) => {
            if let Some(e) = err {
                ctx.error(e);
            }
            sp.say(spoken::notice(n, LOCALE)).await;
            return;
        }
    };

    // Build and prepare the request. At most one downgrade (image dropped) is needed.
    let mut use_image = image.is_some();
    let mut spoken_reason: Option<NotShared> = None;
    let (prepared, opt_in, built) = loop {
        let built = match build_request(
            &text,
            image.as_ref().filter(|_| use_image),
            &transcript,
            not_shared.is_some(),
        ) {
            Ok(b) => b,
            Err(LlmError::Encode(_)) if use_image => {
                use_image = false;
                not_shared = Some(NotShared::ScreenshotTooLarge);
                ctx.firewall(NotShared::ScreenshotTooLarge.status());
                continue;
            }
            Err(e) => {
                ctx.error(llm_turn_error(&e));
                sp.say(spoken::notice(Notice::LlmFailed, LOCALE)).await;
                return;
            }
        };
        // Tell the user why the screen is not (fully) shared, before any waiting.
        if not_shared != spoken_reason {
            if let Some(n) = &not_shared {
                sp.say(spoken::explanation(n, LOCALE)).await;
            }
            spoken_reason = not_shared.clone();
        }
        match ctx
            .buddy
            .prepare_and_consent(&*provider, &built.request)
            .await
        {
            Ok((p, o)) => break (p, o, built),
            Err(CoreError::Llm(LlmError::Encode(_) | LlmError::Config("no_vision_model")))
                if use_image =>
            {
                use_image = false;
                not_shared = Some(NotShared::Screenshot);
                ctx.firewall(NotShared::Screenshot.status());
                continue;
            }
            Err(CoreError::LocalOnly) => {
                ctx.error(simple_turn_error(
                    "local_only",
                    "Local only mode is on: nothing is sent to the cloud.",
                ));
                sp.say(spoken::notice(Notice::LocalOnly, LOCALE)).await;
                return;
            }
            Err(CoreError::Cancelled) => {
                sp.say(spoken::notice(Notice::Cancelled, LOCALE)).await;
                return;
            }
            Err(CoreError::Llm(e)) => {
                ctx.error(llm_turn_error(&e));
                sp.say(spoken::notice(Notice::LlmFailed, LOCALE)).await;
                return;
            }
        }
    };

    ctx.phase(Phase::Thinking);
    let mut deltas = match provider.stream(prepared, opt_in).await {
        Ok(s) => s,
        Err(e) => {
            ctx.error(llm_turn_error(&e));
            sp.say(spoken::notice(Notice::LlmFailed, LOCALE)).await;
            return;
        }
    };

    // POINT context from what the request really contained.
    let displays: Vec<DisplayInfo> = ctx.buddy.capture.displays().unwrap_or_default();
    let node_rect = |id: u32| text.as_ref().and_then(|t| t.node_rect(id));
    let point_ctx = match (&built.geometry, &text) {
        (Some(g), _) => PointContext::Image(*g),
        (None, Some(t)) => PointContext::Node(NodeContext {
            window_rect: t.window_rect(),
            displays: &displays,
            node_rect: &node_rect,
        }),
        (None, None) => PointContext::None,
    };
    let mut points = PointStream::new(point_ctx);
    let mut chunker = SentenceChunker::new();

    let mut failed = false;
    while let Some(item) = deltas.next().await {
        match item {
            Ok(LlmDelta::Text(t)) => {
                let (speakable, events) = points.push(&t);
                emit_reply(ctx, sp, &mut chunker, speakable, events).await;
            }
            Ok(LlmDelta::Done { .. }) => break,
            Err(e) => {
                ctx.error(llm_turn_error(&e));
                failed = true;
                break;
            }
        }
    }
    let (rest, events) = points.finish();
    emit_reply(ctx, sp, &mut chunker, rest, events).await;
    if let Some(last) = chunker.finish() {
        sp.say(&last).await;
    }
    if failed && !sp.said_any {
        sp.say(spoken::notice(Notice::LlmFailed, LOCALE)).await;
    }
}

async fn emit_reply(
    ctx: &TurnCtx,
    sp: &mut Speaker<'_>,
    chunker: &mut SentenceChunker,
    text: String,
    points: Vec<crate::api::PointEvent>,
) {
    if ctx.alive() {
        for p in points {
            ctx.buddy.sink().point(p);
        }
    }
    if text.is_empty() {
        return;
    }
    if ctx.alive() {
        ctx.buddy.sink().reply_delta(ReplyDelta {
            turn_id: ctx.turn_id,
            text: text.clone(),
        });
    }
    for sentence in chunker.push(&text) {
        sp.say(&sentence).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice::fake::FakeVad;

    fn tone(n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect()
    }

    #[test]
    fn trim_keeps_speech_with_pre_roll_and_drops_silence() {
        let mut vad = FakeVad::new();
        let mut a = vec![0.0; 512 * 20];
        a.extend(tone(512 * 6));
        a.extend(vec![0.0; 512 * 40]);
        let out = trim_to_speech(&mut vad, &a).unwrap();
        assert!(!out.is_empty());
        assert!(out.len() < a.len(), "silence must be trimmed");
        assert!(out.len() >= 512 * 6, "speech must be kept");
        // Pre-roll plus speech plus the segmenter hangover, not the whole 40-window tail.
        assert!(out.len() <= 512 * (PRE_ROLL_WINDOWS + 6 + 16));
    }

    #[test]
    fn trim_of_silence_is_empty() {
        let mut vad = FakeVad::new();
        assert!(trim_to_speech(&mut vad, &vec![0.0; 512 * 30])
            .unwrap()
            .is_empty());
        assert!(trim_to_speech(&mut vad, &[]).unwrap().is_empty());
    }

    #[test]
    fn trim_keeps_a_second_utterance_after_a_pause() {
        let mut vad = FakeVad::new();
        let mut a = tone(512 * 6);
        a.extend(vec![0.0; 512 * 20]);
        a.extend(tone(512 * 6));
        let out = trim_to_speech(&mut vad, &a).unwrap();
        assert!(out.len() >= 512 * 6 + 512 * 20 + 512 * 6 - 512 * 2);
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(contains(&r, 0, 0) && contains(&r, 9, 9));
        assert!(!contains(&r, 10, 5) && !contains(&r, -1, 5));
    }
}
