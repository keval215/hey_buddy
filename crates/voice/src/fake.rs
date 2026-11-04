//! Test doubles for the voice traits. Only compiled with the `fakes` feature.

use crate::{
    AudioChunk, AudioInput, AudioOutput, PlaybackHandle, SttEvent, SttOptions, SttProvider,
    Transcript, TtsOptions, TtsProvider, Vad, VoiceError,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use futures::FutureExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Energy-based VAD: probability 1.0 if the window's RMS exceeds `rms_threshold`, else 0.0.
pub struct FakeVad {
    window: usize,
    rms_threshold: f32,
}

impl FakeVad {
    pub fn new() -> Self {
        Self {
            window: 512,
            rms_threshold: 0.01,
        }
    }
    pub fn with_window_len(window: usize) -> Self {
        Self {
            window,
            ..Self::new()
        }
    }
}

impl Default for FakeVad {
    fn default() -> Self {
        Self::new()
    }
}

impl Vad for FakeVad {
    fn window_len(&self) -> usize {
        self.window
    }
    fn speech_prob(&mut self, window: &[f32]) -> Result<f32, VoiceError> {
        if window.is_empty() {
            return Ok(0.0);
        }
        let rms = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
        Ok(if rms > self.rms_threshold { 1.0 } else { 0.0 })
    }
    fn reset(&mut self) {}
}

/// STT that drains all audio, then emits one partial and a final fixed transcript.
pub struct FakeStt {
    text: String,
    samples_seen: Arc<AtomicUsize>,
}

impl FakeStt {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            samples_seen: Arc::new(AtomicUsize::new(0)),
        }
    }
    /// Total samples consumed from audio streams so far.
    pub fn samples_seen(&self) -> usize {
        self.samples_seen.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SttProvider for FakeStt {
    fn id(&self) -> &str {
        "fake-stt"
    }
    async fn transcribe(
        &self,
        audio: BoxStream<'static, AudioChunk>,
        opts: SttOptions,
    ) -> Result<BoxStream<'static, Result<SttEvent, VoiceError>>, VoiceError> {
        let text = self.text.clone();
        let seen = self.samples_seen.clone();
        // Lazy: nothing runs until polled, so dropping the stream cancels.
        let s = stream::once(async move {
            let mut audio = audio;
            while let Some(c) = audio.next().await {
                seen.fetch_add(c.samples.len(), Ordering::SeqCst);
            }
            let first = text.split_whitespace().next().unwrap_or("").to_string();
            vec![
                Ok(SttEvent::Partial(first)),
                Ok(SttEvent::Final(Transcript {
                    text,
                    language: opts.language,
                })),
            ]
        })
        .flat_map(stream::iter)
        .boxed();
        Ok(s)
    }
}

/// TTS that emits a 440 Hz sine chunk (24 kHz) per non-empty text piece,
/// lasting 10 ms per character. Produces audio as soon as each piece arrives.
pub struct FakeTts;

impl FakeTts {
    pub const SAMPLE_RATE: u32 = 24_000;
}

#[async_trait]
impl TtsProvider for FakeTts {
    fn id(&self) -> &str {
        "fake-tts"
    }
    async fn synthesize(
        &self,
        text: BoxStream<'static, String>,
        _opts: TtsOptions,
    ) -> Result<BoxStream<'static, Result<AudioChunk, VoiceError>>, VoiceError> {
        Ok(text
            .filter(|t| futures::future::ready(!t.trim().is_empty()))
            .map(|t| {
                let n = t.chars().count() * (Self::SAMPLE_RATE as usize / 100);
                let samples = (0..n)
                    .map(|i| {
                        0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32
                            / Self::SAMPLE_RATE as f32)
                            .sin()
                    })
                    .collect();
                Ok(AudioChunk {
                    samples,
                    sample_rate: Self::SAMPLE_RATE,
                })
            })
            .boxed())
    }
}

/// Mic that replays fixed chunks on each push-to-talk open.
pub struct FakeAudioIn {
    chunks: Vec<AudioChunk>,
    unavailable: bool,
}

impl FakeAudioIn {
    pub fn new(chunks: Vec<AudioChunk>) -> Self {
        Self {
            chunks,
            unavailable: false,
        }
    }
    /// A mic whose `open_push_to_talk` fails with `DeviceUnavailable`.
    pub fn unavailable() -> Self {
        Self {
            chunks: Vec::new(),
            unavailable: true,
        }
    }
}

impl AudioInput for FakeAudioIn {
    fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError> {
        if self.unavailable {
            return Err(VoiceError::DeviceUnavailable("fake mic unavailable".into()));
        }
        Ok(stream::iter(self.chunks.clone()).boxed())
    }
}

/// Speaker that records played chunks. `play` must be called inside a tokio runtime.
#[derive(Default)]
pub struct FakeAudioOut {
    played: Arc<Mutex<Vec<AudioChunk>>>,
}

impl FakeAudioOut {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recorded(&self) -> Vec<AudioChunk> {
        self.played
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl AudioOutput for FakeAudioOut {
    fn play(
        &self,
        audio: BoxStream<'static, Result<AudioChunk, VoiceError>>,
    ) -> Result<PlaybackHandle, VoiceError> {
        let played = self.played.clone();
        let task = tokio::spawn(async move {
            let mut audio = audio;
            while let Some(Ok(c)) = audio.next().await {
                played.lock().unwrap_or_else(|e| e.into_inner()).push(c);
            }
        });
        let abort = task.abort_handle();
        let done = async move {
            let _ = task.await;
        }
        .boxed();
        Ok(PlaybackHandle::new(move || abort.abort(), done))
    }
}
