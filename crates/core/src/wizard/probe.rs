//! The wizard's probes: machine (RAM, GPU), microphone level and Ollama. Each sits behind a
//! small trait with a fake for tests, and each runs ONLY when the user clicks that step's
//! button (`Buddy::wizard_probe_*`). Constructing a probe does no I/O.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use voice::AudioInput;

use super::state::{MachineInfo, OllamaInfo, OllamaModelDto};

/// Fixed-token errors of the wizard (no paths, no content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WizardError {
    #[error("event not allowed from the window")]
    ForbiddenEvent,
    #[error("not the current step")]
    WrongStep,
    #[error("a voice turn is running")]
    Busy,
    #[error("no microphone check available")]
    MicUnavailable,
    #[error("microphone check failed")]
    MicFailed,
    #[error("microphone check timed out")]
    MicTimeout,
    #[error("could not save wizard state")]
    Io,
    #[error("check failed")]
    ProbeFailed,
}

impl WizardError {
    /// The fixed token the UI maps to text.
    pub fn code(self) -> &'static str {
        match self {
            Self::ForbiddenEvent => "forbidden_event",
            Self::WrongStep => "wrong_step",
            Self::Busy => "busy",
            Self::MicUnavailable => "mic_unavailable",
            Self::MicFailed => "mic_failed",
            Self::MicTimeout => "mic_timeout",
            Self::Io => "io_error",
            Self::ProbeFailed => "probe_failed",
        }
    }
}

// ---------------------------------------------------------------- machine

/// Reads RAM and GPU. Never fails: whatever cannot be read is `None` (and recommends the small
/// profile).
pub trait MachineProbe: Send + Sync {
    fn probe(&self) -> MachineInfo;
}

/// Platforms without a real probe: everything unknown.
pub struct UnknownMachineProbe;

impl MachineProbe for UnknownMachineProbe {
    fn probe(&self) -> MachineInfo {
        MachineInfo::UNKNOWN
    }
}

/// The probe for this OS (Windows: DXGI + GlobalMemoryStatusEx; elsewhere unknown).
pub fn platform_machine_probe() -> Arc<dyn MachineProbe> {
    #[cfg(windows)]
    {
        Arc::new(super::machine_windows::WindowsMachineProbe)
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnknownMachineProbe)
    }
}

/// A fixed answer that counts its calls.
#[cfg(any(test, feature = "fakes"))]
pub struct FakeMachineProbe {
    pub info: MachineInfo,
    pub calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeMachineProbe {
    pub fn new(info: MachineInfo) -> Self {
        Self {
            info,
            calls: Default::default(),
        }
    }
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "fakes"))]
impl MachineProbe for FakeMachineProbe {
    fn probe(&self) -> MachineInfo {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.info.clone()
    }
}

// ---------------------------------------------------------------- microphone

/// A loudness reading. Nothing else about the audio leaves the probe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicLevel {
    /// Largest absolute sample, 0.0 to 1.0.
    pub peak: f32,
    /// Root mean square, 0.0 to 1.0.
    pub rms: f32,
}

/// Quieter than this counts as "no sound heard".
pub const MIC_MIN_PEAK: f32 = 0.02;
/// How much audio the check listens to.
pub const MIC_LISTEN: Duration = Duration::from_millis(1500);
/// The whole check gives up after this.
pub const MIC_TIMEOUT: Duration = Duration::from_secs(5);

impl MicLevel {
    pub fn heard(&self) -> bool {
        self.peak >= MIC_MIN_PEAK
    }
}

/// Opens the microphone for a short push-to-talk-style listen and returns only a level.
#[async_trait]
pub trait MicProbe: Send + Sync {
    async fn level(&self) -> Result<MicLevel, WizardError>;
}

/// The real probe: the app's own [`AudioInput`] (the same one push-to-talk uses). Chunks are
/// folded into a running peak / sum of squares and dropped at once; the stream (and so the
/// microphone) is closed when this returns or is dropped.
pub struct AudioInputProbe {
    input: Arc<dyn AudioInput>,
    listen: Duration,
}

impl AudioInputProbe {
    pub fn new(input: Arc<dyn AudioInput>) -> Self {
        Self::with_listen(input, MIC_LISTEN)
    }
    pub fn with_listen(input: Arc<dyn AudioInput>, listen: Duration) -> Self {
        Self { input, listen }
    }
}

#[async_trait]
impl MicProbe for AudioInputProbe {
    async fn level(&self) -> Result<MicLevel, WizardError> {
        let mut stream = self
            .input
            .open_push_to_talk()
            .map_err(|_| WizardError::MicFailed)?;
        let (mut peak, mut sum_sq, mut n) = (0f32, 0f64, 0u64);
        let mut seconds = 0f64;
        while seconds < self.listen.as_secs_f64() {
            let Some(chunk) = stream.next().await else {
                break;
            };
            for s in &chunk.samples {
                let a = if s.is_finite() { s.abs().min(1.0) } else { 0.0 };
                peak = peak.max(a);
                sum_sq += f64::from(a) * f64::from(a);
            }
            n += chunk.samples.len() as u64;
            if chunk.sample_rate > 0 {
                seconds += chunk.samples.len() as f64 / f64::from(chunk.sample_rate);
            }
            // `chunk` is dropped here: no audio is kept.
        }
        drop(stream);
        if n == 0 {
            return Err(WizardError::MicFailed);
        }
        Ok(MicLevel {
            peak,
            rms: (sum_sq / n as f64).sqrt() as f32,
        })
    }
}

/// A scripted microphone check that counts its calls.
#[cfg(any(test, feature = "fakes"))]
pub struct FakeMicProbe {
    pub result: Result<MicLevel, WizardError>,
    /// Never answers (to test the timeout).
    pub hang: bool,
    pub calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeMicProbe {
    pub fn new(result: Result<MicLevel, WizardError>) -> Self {
        Self {
            result,
            hang: false,
            calls: Default::default(),
        }
    }
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "fakes"))]
#[async_trait]
impl MicProbe for FakeMicProbe {
    async fn level(&self) -> Result<MicLevel, WizardError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.hang {
            std::future::pending::<()>().await;
        }
        self.result
    }
}

// ---------------------------------------------------------------- ollama

/// Asks Ollama which models it has. `Err` = nothing usable answered.
#[async_trait]
pub trait OllamaProbe: Send + Sync {
    async fn tags(&self) -> Result<Vec<String>, WizardError>;
}

/// Builds the wizard's view from a tags answer: which of `wanted` are present. An error means
/// "not running" (the wizard cannot tell not installed from not started).
pub fn ollama_info(answer: Result<Vec<String>, WizardError>, wanted: &[String]) -> OllamaInfo {
    let have = answer.as_ref().ok();
    let present = |tag: &str| {
        have.is_some_and(|h| {
            h.iter()
                .any(|n| n == tag || (!tag.contains(':') && n == &format!("{tag}:latest")))
        })
    };
    OllamaInfo {
        running: have.is_some(),
        models: wanted
            .iter()
            .map(|t| OllamaModelDto {
                tag: t.clone(),
                present: present(t),
            })
            .collect(),
    }
}

/// Longest answer read from Ollama.
const MAX_TAGS_BODY: usize = 1024 * 1024;
const TAGS_TIMEOUT: Duration = Duration::from_secs(3);

/// The real probe: ONE `GET /api/tags` on loopback per click. The base URL goes through the
/// same loopback-only check as the chat provider (`127.0.0.1`, `[::1]` or `localhost`, http,
/// no credentials); no proxy, no redirects.
pub struct LoopbackOllamaProbe {
    tags_url: String,
}

impl LoopbackOllamaProbe {
    /// The default Ollama address, `http://127.0.0.1:11434`. Does no I/O.
    pub fn new() -> Self {
        Self::with_base_url(llm::DEFAULT_OLLAMA_BASE_URL)
            .unwrap_or_else(|_| Self::unreachable_probe())
    }

    /// `base` must pass [`llm::validate_loopback_base_url`].
    pub fn with_base_url(base: &str) -> Result<Self, WizardError> {
        let chat = llm::validate_loopback_base_url(base).map_err(|_| WizardError::ProbeFailed)?;
        let root = chat
            .strip_suffix("/api/chat")
            .ok_or(WizardError::ProbeFailed)?;
        Ok(Self {
            tags_url: format!("{root}/api/tags"),
        })
    }

    fn unreachable_probe() -> Self {
        Self {
            tags_url: String::new(),
        }
    }
}

impl Default for LoopbackOllamaProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct TagsBody {
    models: Vec<TagEntry>,
}
#[derive(Deserialize)]
struct TagEntry {
    name: String,
}

#[async_trait]
impl OllamaProbe for LoopbackOllamaProbe {
    async fn tags(&self) -> Result<Vec<String>, WizardError> {
        if self.tags_url.is_empty() {
            return Err(WizardError::ProbeFailed);
        }
        crate::download::fetch::ensure_crypto_provider();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TAGS_TIMEOUT)
            .build()
            .map_err(|_| WizardError::ProbeFailed)?;
        let mut resp = client
            .get(&self.tags_url)
            .send()
            .await
            .map_err(|_| WizardError::ProbeFailed)?;
        if resp.status() != reqwest::StatusCode::OK {
            return Err(WizardError::ProbeFailed);
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| WizardError::ProbeFailed)? {
            if body.len() + chunk.len() > MAX_TAGS_BODY {
                return Err(WizardError::ProbeFailed);
            }
            body.extend_from_slice(&chunk);
        }
        let parsed: TagsBody =
            serde_json::from_slice(&body).map_err(|_| WizardError::ProbeFailed)?;
        Ok(parsed.models.into_iter().map(|m| m.name).collect())
    }
}

/// A scripted Ollama answer that counts its calls.
#[cfg(any(test, feature = "fakes"))]
pub struct FakeOllamaProbe {
    pub answer: Result<Vec<String>, WizardError>,
    pub calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeOllamaProbe {
    pub fn new(answer: Result<Vec<String>, WizardError>) -> Self {
        Self {
            answer,
            calls: Default::default(),
        }
    }
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "fakes"))]
#[async_trait]
impl OllamaProbe for FakeOllamaProbe {
    async fn tags(&self) -> Result<Vec<String>, WizardError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.answer.clone()
    }
}
