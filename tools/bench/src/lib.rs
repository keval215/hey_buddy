//! Latency benchmark: speech end -> first audio out.
//!
//! Pipeline: synthetic audio -> VAD + Segmenter -> STT -> LLM (always a bench-local fake)
//! -> SentenceChunker -> TTS -> audio sink. Stages use real models when the matching cargo
//! feature is on and the model is present; otherwise fakes, reported per stage.
//! Reports contain timings and component labels only: never audio or text.

use futures::executor::block_on;
use futures::stream::{self, BoxStream, StreamExt};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use voice::fake::{FakeStt, FakeTts, FakeVad};
use voice::{
    AudioChunk, SegmentEvent, Segmenter, SegmenterConfig, SentenceChunker, SttEvent, SttOptions,
    SttProvider, TtsOptions, TtsProvider, Vad, VoiceError,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const TARGET_APPLE_SILICON_MS: f64 = 1000.0;
pub const TARGET_CPU_LAPTOP_MS: f64 = 1500.0;
const SCRIPT: [&str; 6] = [
    "Sure, ",
    "I can ",
    "help with that. ",
    "Let me ",
    "take a look ",
    "at your screen.",
];
/// Deltas needed before the first sentence is complete (the first sentence ends in script[2]).
pub const DELTAS_BEFORE_FIRST_SENTENCE: u64 = 3;
const FAKE_TRANSCRIPT: &str = "what is on my screen";
const RATE: u32 = 16_000;

#[derive(Debug, Clone)]
pub struct Config {
    pub models_dir: Option<PathBuf>,
    pub stt_model: Option<String>,
    pub force_fake: bool,
    pub stt_delay_ms: u64,
    pub llm_first_token_delay_ms: u64,
    pub llm_delta_delay_ms: u64,
    pub tts_delay_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            models_dir: None,
            stt_model: None,
            force_fake: false,
            stt_delay_ms: 20,
            llm_first_token_delay_ms: 30,
            llm_delta_delay_ms: 5,
            tts_delay_ms: 20,
        }
    }
}

impl Config {
    /// Minimum total latency (ms) the configured fake delays force. Only exact when every
    /// delayed stage is fake; real stages add their own time on top.
    pub fn delay_floor_ms(&self) -> u64 {
        self.stt_delay_ms
            + self.llm_first_token_delay_ms
            + (DELTAS_BEFORE_FIRST_SENTENCE - 1) * self.llm_delta_delay_ms
            + self.tts_delay_ms
    }
}

pub struct Parts {
    pub vad: Box<dyn Vad>,
    pub stt: Box<dyn SttProvider>,
    pub tts: Box<dyn TtsProvider>,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub stage: String,
    /// "real" or "fake"
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageTiming {
    pub name: String,
    pub ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Host {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Targets {
    pub apple_silicon_ms: f64,
    pub cpu_laptop_ms: f64,
    /// Which target applies to this host: "apple_silicon" or "cpu_laptop".
    pub applies: String,
    pub target_ms: f64,
    /// total < target_ms. Only meaningful when `meaningful` is true.
    pub pass: bool,
    /// True only when both STT and TTS are real. With fakes, `pass` says nothing.
    pub meaningful: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub host: Host,
    pub components: Vec<Component>,
    pub stages: Vec<StageTiming>,
    pub total_ms: f64,
    pub vad_detected_speech: bool,
    pub targets: Targets,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn comp(stage: &str, real: bool, id: &str) -> Component {
    Component {
        stage: stage.into(),
        kind: if real { "real" } else { "fake" }.into(),
        id: id.into(),
    }
}

// ---- fake-stage wrappers adding artificial delay ----

struct DelayStt {
    inner: Box<dyn SttProvider>,
    delay: Duration,
}

#[async_trait::async_trait]
impl SttProvider for DelayStt {
    fn id(&self) -> &str {
        self.inner.id()
    }
    async fn transcribe(
        &self,
        audio: BoxStream<'static, AudioChunk>,
        opts: SttOptions,
    ) -> std::result::Result<
        BoxStream<'static, std::result::Result<SttEvent, VoiceError>>,
        VoiceError,
    > {
        let delay = self.delay;
        let s = self.inner.transcribe(audio, opts).await?;
        Ok(s.map(move |e| {
            if matches!(e, Ok(SttEvent::Final(_))) {
                std::thread::sleep(delay);
            }
            e
        })
        .boxed())
    }
}

struct DelayTts {
    inner: Box<dyn TtsProvider>,
    delay: Duration,
}

#[async_trait::async_trait]
impl TtsProvider for DelayTts {
    fn id(&self) -> &str {
        self.inner.id()
    }
    async fn synthesize(
        &self,
        text: BoxStream<'static, String>,
        opts: TtsOptions,
    ) -> std::result::Result<
        BoxStream<'static, std::result::Result<AudioChunk, VoiceError>>,
        VoiceError,
    > {
        let delay = self.delay;
        let mut first = true;
        let s = self.inner.synthesize(text, opts).await?;
        Ok(s.map(move |c| {
            if std::mem::take(&mut first) {
                std::thread::sleep(delay);
            }
            c
        })
        .boxed())
    }
}

// ---- stage resolution (real when feature + model present, else fake) ----

#[cfg_attr(
    not(any(
        feature = "vad-silero",
        feature = "stt-whisper",
        feature = "tts-phonon"
    )),
    allow(dead_code)
)]
enum Resolved<T> {
    Real(T),
    Fake(String),
}

#[cfg_attr(
    not(any(
        feature = "vad-silero",
        feature = "stt-whisper",
        feature = "tts-phonon"
    )),
    allow(dead_code)
)]
fn load_registry() -> Result<voice::registry::Registry> {
    Ok(voice::registry::Registry::from_json_str(include_str!(
        "../../../models/registry.json"
    ))?)
}

/// Only `ModelMissing` falls back to a fake (L-007); other errors surface.
#[cfg_attr(
    not(any(
        feature = "vad-silero",
        feature = "stt-whisper",
        feature = "tts-phonon"
    )),
    allow(dead_code)
)]
fn missing_or_err<T>(e: VoiceError, what: &str) -> Result<Resolved<T>> {
    match e {
        VoiceError::ModelMissing { .. } => Ok(Resolved::Fake(format!("{what} model not found"))),
        other => Err(Box::new(other)),
    }
}

#[cfg(feature = "vad-silero")]
fn real_vad(dir: &Path) -> Result<Resolved<Box<dyn Vad>>> {
    let reg = load_registry()?;
    let id = reg
        .default_for(voice::registry::Task::Vad)
        .ok_or("no default VAD in registry")?
        .id
        .clone();
    match voice::vad_silero::SileroVad::from_registry(&reg, &id, dir) {
        Ok(v) => Ok(Resolved::Real(Box::new(v))),
        Err(e) => missing_or_err(e, "VAD"),
    }
}
#[cfg(not(feature = "vad-silero"))]
fn real_vad(_dir: &Path) -> Result<Resolved<Box<dyn Vad>>> {
    Ok(Resolved::Fake("feature vad-silero not enabled".into()))
}

#[cfg(feature = "stt-whisper")]
fn real_stt(dir: &Path, model: Option<&str>) -> Result<Resolved<Box<dyn SttProvider>>> {
    let reg = load_registry()?;
    let id = match model {
        Some(m) => m.to_string(),
        None => reg
            .default_for(voice::registry::Task::Stt)
            .ok_or("no default STT in registry")?
            .id
            .clone(),
    };
    match voice::stt_whisper::WhisperStt::from_registry(&reg, &id, false, dir) {
        Ok(v) => Ok(Resolved::Real(Box::new(v))),
        Err(e) => missing_or_err(e, "STT"),
    }
}
#[cfg(not(feature = "stt-whisper"))]
fn real_stt(_dir: &Path, _model: Option<&str>) -> Result<Resolved<Box<dyn SttProvider>>> {
    Ok(Resolved::Fake("feature stt-whisper not enabled".into()))
}

#[cfg(feature = "tts-phonon")]
fn real_tts(dir: &Path) -> Result<Resolved<Box<dyn TtsProvider>>> {
    let reg = load_registry()?;
    match voice::tts_phonon::from_registry(&reg, dir) {
        Ok(v) => Ok(Resolved::Real(Box::new(v))),
        Err(e) => missing_or_err(e, "TTS"),
    }
}
#[cfg(not(feature = "tts-phonon"))]
fn real_tts(_dir: &Path) -> Result<Resolved<Box<dyn TtsProvider>>> {
    Ok(Resolved::Fake("feature tts-phonon not enabled".into()))
}

fn resolve_dir<T>(why: &str, r: Option<Result<Resolved<T>>>) -> Result<Resolved<T>> {
    r.unwrap_or_else(|| Ok(Resolved::Fake(why.into())))
}

/// Build the stages and one "using real/fake X" line per stage (plus the LLM line).
pub fn build_parts(cfg: &Config) -> Result<(Parts, Vec<String>)> {
    let mut notes = Vec::new();
    let mut components = Vec::new();
    let dir = if cfg.force_fake {
        None
    } else {
        cfg.models_dir.as_deref()
    };
    let why_no_dir = if cfg.force_fake {
        "--force-fake"
    } else {
        "no models dir (HEY_BUDDY_MODELS_DIR / --models-dir)"
    };
    let vad = match resolve_dir(why_no_dir, dir.map(real_vad))? {
        Resolved::Real(v) => {
            notes.push("using real VAD (silero)".into());
            components.push(comp("vad", true, "silero-vad"));
            v
        }
        Resolved::Fake(why) => {
            notes.push(format!("using fake VAD ({why})"));
            components.push(comp("vad", false, "fake-vad"));
            Box::new(FakeVad::new()) as Box<dyn Vad>
        }
    };
    let stt = match resolve_dir(
        why_no_dir,
        dir.map(|d| real_stt(d, cfg.stt_model.as_deref())),
    )? {
        Resolved::Real(s) => {
            notes.push(format!("using real STT ({})", s.id()));
            components.push(comp("stt", true, s.id()));
            s
        }
        Resolved::Fake(why) => {
            notes.push(format!("using fake STT ({why})"));
            components.push(comp("stt", false, "fake-stt"));
            Box::new(DelayStt {
                inner: Box::new(FakeStt::new(FAKE_TRANSCRIPT)),
                delay: Duration::from_millis(cfg.stt_delay_ms),
            }) as Box<dyn SttProvider>
        }
    };
    notes.push("using fake LLM (scripted deltas, artificial delay)".into());
    components.push(comp("llm", false, "bench-fake-llm"));
    let tts = match resolve_dir(why_no_dir, dir.map(real_tts))? {
        Resolved::Real(t) => {
            notes.push(format!("using real TTS ({})", t.id()));
            components.push(comp("tts", true, t.id()));
            t
        }
        Resolved::Fake(why) => {
            notes.push(format!("using fake TTS ({why})"));
            components.push(comp("tts", false, "fake-tts"));
            Box::new(DelayTts {
                inner: Box::new(FakeTts),
                delay: Duration::from_millis(cfg.tts_delay_ms),
            }) as Box<dyn TtsProvider>
        }
    };
    notes.push("using fake audio out (chunks are timestamped, not played)".into());
    Ok((
        Parts {
            vad,
            stt,
            tts,
            components,
        },
        notes,
    ))
}

// ---- the pipeline ----

/// Synthetic input: 0.3 s silence, 1 s tone, 1 s silence (16 kHz mono). No real speech, so a
/// real VAD may not fire; see `vad_detected_speech`.
fn synthetic_audio() -> Vec<f32> {
    let mut a = vec![0.0; RATE as usize * 3 / 10];
    a.extend(
        (0..RATE as usize)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / RATE as f32).sin()),
    );
    a.extend(std::iter::repeat_n(0.0, RATE as usize));
    a
}

type Stamp = Arc<Mutex<Option<Instant>>>;

fn stamp_now(s: &Stamp) {
    let mut g = s.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Instant::now);
}

fn stamp_get(s: &Stamp, what: &str) -> Result<Instant> {
    s.lock()
        .unwrap_or_else(|e| e.into_inner())
        .ok_or_else(|| format!("pipeline produced no {what}").into())
}

pub fn run(cfg: &Config, mut parts: Parts) -> Result<Report> {
    let audio = synthetic_audio();

    // VAD + segmenter -> speech end.
    let win = parts.vad.window_len().max(1);
    let mut seg = Segmenter::new(SegmenterConfig::default());
    let mut end_at = None;
    for (i, w) in audio.chunks_exact(win).enumerate() {
        let p = parts.vad.speech_prob(w)?;
        if seg.push(p) == Some(SegmentEvent::SpeechEnd) {
            end_at = Some((i + 1) * win);
            break;
        }
    }
    let detected = end_at.is_some();
    // Fail-soft only for the bench: a real VAD may ignore a synthetic tone.
    let end = end_at.unwrap_or(audio.len());
    let segment = audio[..end].to_vec();
    let t_end = Instant::now();

    // STT.
    let input = stream::iter(vec![AudioChunk {
        samples: segment,
        sample_rate: RATE,
    }])
    .boxed();
    let events = block_on(parts.stt.transcribe(input, SttOptions::default()))?;
    let t_stt = block_on(async move {
        let mut events = events;
        while let Some(e) = events.next().await {
            if let SttEvent::Final(_) = e? {
                return Ok(Instant::now());
            }
        }
        Err(VoiceError::Backend("stt: no final transcript".into()))
    })?;

    // Fake LLM -> chunker -> TTS -> audio sink.
    let t_llm: Stamp = Arc::default();
    let t_tts: Stamp = Arc::default();
    let t_out: Stamp = Arc::default();
    let first_delay = Duration::from_millis(cfg.llm_first_token_delay_ms);
    let delta_delay = Duration::from_millis(cfg.llm_delta_delay_ms);
    let llm_stamp = t_llm.clone();
    let deltas = stream::unfold(0usize, move |i| {
        let llm_stamp = llm_stamp.clone();
        async move {
            let d = SCRIPT.get(i)?;
            std::thread::sleep(if i == 0 { first_delay } else { delta_delay });
            stamp_now(&llm_stamp);
            Some((d.to_string(), i + 1))
        }
    });
    let sentences = stream::unfold(
        (
            deltas.boxed(),
            SentenceChunker::new(),
            Vec::<String>::new(),
            false,
        ),
        |(mut d, mut c, mut q, mut done)| async move {
            loop {
                if !q.is_empty() {
                    let s = q.remove(0);
                    return Some((s, (d, c, q, done)));
                }
                if done {
                    return None;
                }
                match d.next().await {
                    Some(x) => q = c.push(&x),
                    None => {
                        done = true;
                        q.extend(c.finish());
                    }
                }
            }
        },
    )
    .boxed();
    let mut audio_out = block_on(parts.tts.synthesize(sentences, TtsOptions::default()))?;
    let (tts_stamp, out_stamp) = (t_tts.clone(), t_out.clone());
    let mut sink: Vec<AudioChunk> = Vec::new(); // fake audio out
    block_on(async {
        while let Some(c) = audio_out.next().await {
            let c = c?;
            stamp_now(&tts_stamp);
            if sink.is_empty() {
                sink.push(c);
                stamp_now(&out_stamp);
                break; // first audio reached; later chunks do not affect the metric
            }
        }
        Ok::<(), VoiceError>(())
    })?;
    drop(audio_out);

    let (t_llm, t_tts, t_out) = (
        stamp_get(&t_llm, "LLM token")?,
        stamp_get(&t_tts, "TTS chunk")?,
        stamp_get(&t_out, "audio")?,
    );
    let stages = vec![
        StageTiming {
            name: "speech_end_to_stt_final".into(),
            ms: ms(t_stt.saturating_duration_since(t_end)),
        },
        StageTiming {
            name: "stt_final_to_first_llm_token".into(),
            ms: ms(t_llm.saturating_duration_since(t_stt)),
        },
        StageTiming {
            name: "first_llm_token_to_first_tts_chunk".into(),
            ms: ms(t_tts.saturating_duration_since(t_llm)),
        },
    ];
    let total_ms = ms(t_out.saturating_duration_since(t_end));
    let real = |s: &str| {
        parts
            .components
            .iter()
            .any(|c| c.stage == s && c.kind == "real")
    };
    let meaningful = real("stt") && real("tts");
    let apple = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let target_ms = if apple {
        TARGET_APPLE_SILICON_MS
    } else {
        TARGET_CPU_LAPTOP_MS
    };
    Ok(Report {
        schema_version: 1,
        host: Host {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        },
        components: parts.components,
        stages,
        total_ms,
        vad_detected_speech: detected,
        targets: Targets {
            apple_silicon_ms: TARGET_APPLE_SILICON_MS,
            cpu_laptop_ms: TARGET_CPU_LAPTOP_MS,
            applies: if apple { "apple_silicon" } else { "cpu_laptop" }.into(),
            target_ms,
            pass: total_ms < target_ms,
            meaningful,
            note: if meaningful {
                "STT and TTS are real; pass/fail is meaningful (LLM is still a fake)".into()
            } else {
                "STT and/or TTS are fake; pass/fail is NOT meaningful".into()
            },
        },
    })
}

pub fn render_table(r: &Report) -> String {
    let mut s = String::new();
    s.push_str(&format!("host: {} / {}\n", r.host.os, r.host.arch));
    s.push_str("stage                                 ms\n");
    s.push_str("------------------------------------------\n");
    for st in &r.stages {
        s.push_str(&format!("{:<34} {:>8.1}\n", st.name, st.ms));
    }
    s.push_str(&format!(
        "{:<34} {:>8.1}\n",
        "speech_end_to_first_audio (total)", r.total_ms
    ));
    let t = &r.targets;
    s.push_str(&format!(
        "target ({}): < {:.0} ms -> {}{}\n",
        t.applies,
        t.target_ms,
        if t.pass { "PASS" } else { "FAIL" },
        if t.meaningful {
            ""
        } else {
            " (not meaningful: STT/TTS are fake)"
        }
    ));
    s
}
