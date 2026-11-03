//! Phonon 31m TTS via the `ptts` crate. Feature `tts-phonon`.
//!
//! Only Phonon 31m (Apache-2.0, English) is referenced. Phonon 40m is CC BY-NC and is never
//! a default or loaded from here by id. The checkpoint is a directory
//! (`config.json`, weights, `tokenizer.json`, voices) at `<models_dir>/phonon-31m/`.
//! Nothing is downloaded: `ptts` is built without its `hub` feature.
//! Errors are fixed strings; text and audio never appear in them.

use crate::tts_stream::{PcmIter, StreamingTts, TtsCore};
use crate::VoiceError;
use ptts::checkpoint::{Checkpoint, ResolveOptions};
use ptts::preprocess::Lang;
use ptts::synth::{DeviceKind, SpeechOptions, Synth};
use std::path::Path;

/// Registry id of the only Phonon model this adapter targets.
pub const PHONON_MODEL_ID: &str = "phonon-31m";

fn backend(what: &str) -> VoiceError {
    VoiceError::Backend(format!("phonon: {what}"))
}

/// A loaded Phonon synthesiser (one sentence at a time).
pub struct PhononCore {
    synth: Synth,
    rate: u32,
}

impl PhononCore {
    fn load(dir: &Path) -> Result<Self, VoiceError> {
        if !dir.join("config.json").is_file() {
            return Err(VoiceError::ModelMissing {
                path: dir.to_path_buf(),
            });
        }
        let checkpoint = Checkpoint::resolve(dir, ResolveOptions::default())
            .map_err(|_| backend("cannot read checkpoint"))?;
        let mut synth = checkpoint
            .builder(Lang::En)
            .device(DeviceKind::Cpu)
            .build()
            .map_err(|_| backend("cannot load model"))?;
        checkpoint.register_voices(&mut synth);
        let rate = synth.sample_rate();
        Ok(Self { synth, rate })
    }
}

impl TtsCore for PhononCore {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn synth(&self, sentence: &str, voice: Option<&str>) -> Result<PcmIter, VoiceError> {
        let stream = match voice {
            None => self.synth.stream(sentence),
            Some(v) => self
                .synth
                .stream_with(sentence, &SpeechOptions::default().voice(v)),
        }
        .map_err(|_| backend("synthesis failed (unknown voice?)"))?;
        Ok(Box::new(
            stream.map(|r| r.map_err(|_| backend("synthesis failed"))),
        ))
    }
}

pub type PhononTts = StreamingTts<PhononCore>;

/// Load Phonon 31m from a checkpoint directory. Missing -> `ModelMissing`.
pub fn from_dir(dir: &Path) -> Result<PhononTts, VoiceError> {
    Ok(StreamingTts::new(PHONON_MODEL_ID, PhononCore::load(dir)?))
}

/// Load via the registry: `<models_dir>/phonon-31m/`.
pub fn from_registry(
    registry: &crate::registry::Registry,
    models_dir: &Path,
) -> Result<PhononTts, VoiceError> {
    from_dir(&registry.model_dir(PHONON_MODEL_ID, models_dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TtsOptions, TtsProvider};
    use futures::StreamExt;

    #[test]
    fn missing_dir_is_model_missing() {
        let p = Path::new("/nonexistent/phonon-31m");
        assert!(matches!(
            from_dir(p),
            Err(VoiceError::ModelMissing { path }) if path == p
        ));
    }

    #[test]
    fn default_tts_model_is_phonon_31m() {
        let reg =
            crate::registry::Registry::from_json_str(include_str!("../../../models/registry.json"))
                .unwrap();
        let d = reg.default_for(crate::registry::Task::Tts).unwrap();
        assert_eq!(d.id, PHONON_MODEL_ID);
    }

    /// Runs only if the checkpoint dir exists (`PHONON_MODEL_DIR`, or
    /// `$HEY_BUDDY_MODELS_DIR/phonon-31m`). ModelMissing/unset skips; other errors fail (L-007).
    #[tokio::test]
    async fn synthesises_short_sentence_when_model_present() {
        let dir = std::env::var_os("PHONON_MODEL_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HEY_BUDDY_MODELS_DIR")
                    .map(|d| Path::new(&d).join(PHONON_MODEL_ID))
            });
        let Some(dir) = dir else {
            eprintln!(
                "SKIP phonon integration test: PHONON_MODEL_DIR / HEY_BUDDY_MODELS_DIR unset"
            );
            return;
        };
        let tts = match from_dir(&dir) {
            Ok(t) => t,
            Err(VoiceError::ModelMissing { .. }) => {
                eprintln!("SKIP phonon integration test: model files missing");
                return;
            }
            Err(e) => panic!("phonon load failed: {e}"),
        };
        let text = futures::stream::iter(vec!["Hello there.".to_string()]);
        let audio: Vec<_> = tts
            .synthesize(Box::pin(text), TtsOptions::default())
            .await
            .unwrap()
            .collect()
            .await;
        let total: usize = audio
            .iter()
            .map(|c| c.as_ref().unwrap().samples.len())
            .sum();
        assert!(total > 1000, "expected audio samples");
        assert!(audio
            .iter()
            .all(|c| c.as_ref().unwrap().samples.iter().all(|s| s.is_finite())));
    }
}
