//! Local speech-to-text via whisper.cpp (`whisper-rs`). Feature `stt-whisper`.
//!
//! Model file: `<models_dir>/<id>.bin` (id from the registry, e.g. `whisper-large-v3-turbo`).
//! Nothing is downloaded. Errors never contain transcript or audio.

use crate::registry::Registry;
use crate::{AudioChunk, BoxStream, SttEvent, SttOptions, SttProvider, Transcript, VoiceError};
use async_trait::async_trait;
use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Longest utterance accepted (30 s at 16 kHz), a whisper window; longer input is rejected
/// rather than silently truncated.
const MAX_SAMPLES: usize = 30 * 16_000;

fn backend(what: &str) -> VoiceError {
    VoiceError::Backend(format!("whisper: {what}"))
}

pub struct WhisperStt {
    id: String,
    ctx: Arc<WhisperContext>,
}

impl WhisperStt {
    /// Load a model from `path`. Missing file -> `ModelMissing`.
    pub fn from_path(id: &str, path: &Path) -> Result<Self, VoiceError> {
        if !path.is_file() {
            return Err(VoiceError::ModelMissing {
                path: path.to_path_buf(),
            });
        }
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|_| backend("cannot load model"))?;
        Ok(Self {
            id: id.to_string(),
            ctx: Arc::new(ctx),
        })
    }

    /// Load via the registry; file is `<models_dir>/<id>.bin`.
    pub fn from_registry(
        registry: &Registry,
        id: &str,
        user_accepted_licence: bool,
        models_dir: &Path,
    ) -> Result<Self, VoiceError> {
        let p = registry.model_path(id, user_accepted_licence, models_dir, "bin")?;
        Self::from_path(id, &p)
    }

    fn run(
        ctx: &WhisperContext,
        samples: &[f32],
        lang: Option<&str>,
    ) -> Result<Transcript, VoiceError> {
        let mut state = ctx
            .create_state()
            .map_err(|_| backend("cannot create state"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(lang);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);
        state
            .full(params, samples)
            .map_err(|_| backend("inference failed"))?;
        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(seg) = state.get_segment(i) {
                let t = seg.to_str_lossy().map_err(|_| backend("bad segment"))?;
                text.push_str(&t);
            }
        }
        let language = lang.map(str::to_string).or_else(|| {
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string)
        });
        Ok(Transcript {
            text: text.trim().to_string(),
            language,
        })
    }
}

#[async_trait]
impl SttProvider for WhisperStt {
    fn id(&self) -> &str {
        &self.id
    }

    async fn transcribe(
        &self,
        mut audio: BoxStream<'static, AudioChunk>,
        opts: SttOptions,
    ) -> Result<BoxStream<'static, Result<SttEvent, VoiceError>>, VoiceError> {
        let mut samples: Vec<f32> = Vec::new();
        while let Some(chunk) = audio.next().await {
            if chunk.sample_rate != 16_000 {
                return Err(VoiceError::InvalidAudio(format!(
                    "stt expects 16000 Hz mono, got {} Hz",
                    chunk.sample_rate
                )));
            }
            if samples.len() + chunk.samples.len() > MAX_SAMPLES {
                return Err(VoiceError::InvalidAudio(
                    "utterance longer than 30 s".to_string(),
                ));
            }
            samples.extend_from_slice(&chunk.samples);
        }
        if samples.is_empty() {
            return Ok(futures::stream::empty().boxed());
        }
        let ctx = Arc::clone(&self.ctx);
        let (tx, rx) = futures::channel::oneshot::channel();
        // Inference is blocking and CPU heavy: run it off the async executor.
        std::thread::spawn(move || {
            let r = Self::run(&ctx, &samples, opts.language.as_deref());
            let _ = tx.send(r);
        });
        let fut = async move {
            match rx.await {
                Ok(Ok(t)) => Ok(SttEvent::Final(t)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(VoiceError::Cancelled),
            }
        };
        Ok(futures::stream::once(fut).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn missing_model_is_model_missing() {
        let p = Path::new("/nonexistent/whisper-small.bin");
        assert!(matches!(
            WhisperStt::from_path("whisper-small", p),
            Err(VoiceError::ModelMissing { path }) if path == p
        ));
    }

    #[test]
    fn registry_missing_model_is_model_missing() {
        let reg = Registry::from_json_str(include_str!("../../../models/registry.json")).unwrap();
        let dir = Path::new("/nonexistent-models-dir");
        let e = WhisperStt::from_registry(&reg, "whisper-small", false, dir).err();
        assert!(matches!(e, Some(VoiceError::ModelMissing { .. })));
    }

    /// Fixture: 0.5 s of synthetic tones at 16 kHz mono (no voice). Deterministic.
    fn synth_samples() -> Vec<i16> {
        (0..8000)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let f = if i < 4000 { 440.0 } else { 660.0 };
                ((t * f * std::f32::consts::TAU).sin() * 0.3 * i16::MAX as f32) as i16
            })
            .collect()
    }

    fn fixture_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic_tone_16k.wav")
    }

    fn wav_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for s in synth_samples() {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        buf.into_inner()
    }

    /// Regenerate with: cargo test -p voice --features stt-whisper regenerate_fixture -- --ignored
    #[test]
    #[ignore]
    fn regenerate_fixture() {
        std::fs::create_dir_all(fixture_path().parent().unwrap()).unwrap();
        std::fs::write(fixture_path(), wav_bytes()).unwrap();
    }

    #[test]
    fn committed_fixture_matches_generator() {
        assert_eq!(std::fs::read(fixture_path()).unwrap(), wav_bytes());
    }

    /// Runs only if `$HEY_BUDDY_MODELS_DIR/<id>.bin` exists (id: WHISPER_MODEL_ID or
    /// `whisper-small`). The fixture is a synthetic tone, so no particular text is expected;
    /// we check the pipeline runs and yields a Final event.
    #[test]
    fn transcribes_fixture_when_model_present() {
        let id = std::env::var("WHISPER_MODEL_ID").unwrap_or_else(|_| "whisper-small".into());
        let dir = std::env::var_os("HEY_BUDDY_MODELS_DIR");
        let reg = Registry::from_json_str(include_str!("../../../models/registry.json")).unwrap();
        let stt = match dir
            .as_ref()
            .map(|d| WhisperStt::from_registry(&reg, &id, false, Path::new(d)))
        {
            Some(Ok(s)) => s,
            _ => {
                eprintln!(
                    "SKIP whisper integration test: model {id}.bin not found \
                     (set HEY_BUDDY_MODELS_DIR)"
                );
                return;
            }
        };
        let mut r = hound::WavReader::open(fixture_path()).unwrap();
        let samples: Vec<f32> = r
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / i16::MAX as f32)
            .collect();
        let chunk = AudioChunk {
            samples,
            sample_rate: 16_000,
        };
        let mut out = block_on(stt.transcribe(
            futures::stream::iter(vec![chunk]).boxed(),
            SttOptions {
                language: Some("en".into()),
            },
        ))
        .unwrap();
        let ev = block_on(out.next()).expect("event").expect("ok");
        assert!(matches!(ev, SttEvent::Final(_)));
    }
}
