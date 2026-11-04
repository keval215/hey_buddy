//! The real voice adapters (feature `real-voice`, OFF by default): whisper.cpp STT (CPU),
//! Silero VAD (onnxruntime, needs `ORT_DYLIB_PATH`), Phonon 31m TTS (CPU) and cpal
//! microphone / speaker, chosen from the active registry profile. Model files are looked up in
//! the models directory and NEVER downloaded; a missing file is reported with the model id and
//! the exact path it is expected at.
//!
//! Layout of the models directory (default `<app data dir>/models`, override with the
//! environment variable `HEY_BUDDY_MODELS_DIR`):
//! - `<stt id>.bin`, e.g. `whisper-small.bin` (whisper.cpp ggml model)
//! - `silero-vad-v5.onnx`
//! - `phonon-31m/` (Phonon checkpoint directory)
//!
//! UNTESTED on this machine beyond compiling: needs real models and audio devices.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use voice::mic_cpal::CpalMic;
use voice::registry::{Registry, Task};
use voice::speaker_cpal::CpalSpeaker;
use voice::stt_whisper::WhisperStt;
use voice::vad_silero::SileroVad;
use voice::{tts_phonon, SttProvider, TtsProvider, Vad, VoiceError};

use crate::engine_slot::{EngineLoader, SwapFailure, TOKEN_LOAD_FAILED};
use crate::settings::builtin_registry;
use crate::turn::VadFactory;

pub use crate::download::{models_dir, SharedDir, MODELS_DIR_ENV};

/// One expected model that is not on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingModel {
    pub id: String,
    pub path: PathBuf,
}

impl MissingModel {
    /// Display text for the status notices.
    pub fn notice(&self) -> String {
        format!(
            "Missing model file for {}: place it at {} (nothing is downloaded automatically).",
            self.id,
            self.path.display()
        )
    }
}

fn vad_id(reg: &Registry) -> String {
    reg.default_for(Task::Vad)
        .map_or_else(|| "silero-vad-v5".to_string(), |m| m.id.clone())
}

/// Which of the profile's STT, VAD and TTS model files are missing under `dir`.
pub fn missing_models(profile: &str, dir: &Path) -> Vec<MissingModel> {
    let reg = builtin_registry();
    let mut out = Vec::new();
    let Some(p) = reg.profile(profile) else {
        return out;
    };
    let mut want = |id: &str, path: PathBuf, dir_expected: bool| {
        let ok = if dir_expected {
            path.is_dir()
        } else {
            path.is_file()
        };
        if !ok {
            out.push(MissingModel {
                id: id.to_string(),
                path,
            });
        }
    };
    want(
        &p.stt.model,
        dir.join(format!("{}.bin", p.stt.model)),
        false,
    );
    let vad = vad_id(reg);
    want(&vad, dir.join(format!("{vad}.onnx")), false);
    want(&p.tts.model, dir.join(&p.tts.model), true);
    out
}

/// Builds the swappable engines from the models directory (`request_engine_swap`, ticket
/// S2-2). Startup load is just the first swap. Slow: runs on the swap thread.
pub struct RealLoader {
    dir: SharedDir,
}

impl RealLoader {
    pub fn new(dir: PathBuf) -> Self {
        Self::shared(Arc::new(std::sync::Mutex::new(dir)))
    }

    /// A loader that follows the models directory setting (S2-7).
    pub fn shared(dir: SharedDir) -> Self {
        Self { dir }
    }

    fn dir(&self) -> PathBuf {
        self.dir.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// Map an adapter error to the fixed token (+ expected path for a missing file).
fn failure(e: &VoiceError) -> SwapFailure {
    match e {
        VoiceError::ModelMissing { path } => SwapFailure::missing(path.display().to_string()),
        _ => SwapFailure::new(TOKEN_LOAD_FAILED),
    }
}

impl EngineLoader for RealLoader {
    fn load_stt(&self, model_id: &str) -> Result<Arc<dyn SttProvider>, SwapFailure> {
        let stt = WhisperStt::from_registry(builtin_registry(), model_id, &self.dir())
            .map_err(|e| failure(&e))?;
        Ok(Arc::new(stt))
    }

    fn load_tts(&self, model_id: &str) -> Result<Arc<dyn TtsProvider>, SwapFailure> {
        // Phonon 31m is the only TTS the real build loads (Kokoro is a later ticket).
        if model_id != "phonon-31m" {
            return Err(SwapFailure::new(TOKEN_LOAD_FAILED));
        }
        let tts =
            tts_phonon::from_registry(builtin_registry(), &self.dir()).map_err(|e| failure(&e))?;
        Ok(Arc::new(tts))
    }
}

/// The engine pieces that do not change with the profile: platform firewall, `deny`, cpal
/// mic and speaker, the Silero VAD factory and `providers`. STT and TTS are not here: they
/// are loaded into the slots with `Buddy::request_engine_swap`. Slow (loads the VAD): call off
/// the UI thread.
pub fn base_deps(
    dir: SharedDir,
    deny: Arc<capture::DenyList>,
    providers: Arc<dyn crate::turn::ProviderSource>,
    runtime: tokio::runtime::Handle,
) -> Result<crate::turn::EngineDeps, VoiceError> {
    let reg = builtin_registry();
    let vad_model = vad_id(reg);
    // Load once now so a missing file or onnxruntime library is reported at startup; each
    // turn gets a fresh detector (it keeps per-utterance state).
    let current = || dir.lock().unwrap_or_else(|p| p.into_inner()).clone();
    drop(SileroVad::from_registry(reg, &vad_model, &current())?);
    let vad: VadFactory = Arc::new(move || {
        let now = dir.lock().unwrap_or_else(|p| p.into_inner()).clone();
        Ok(Box::new(SileroVad::from_registry(
            builtin_registry(),
            &vad_model,
            &now,
        )?) as Box<dyn Vad>)
    });
    Ok(crate::turn::EngineDeps {
        firewall: crate::setup::platform_firewall(),
        deny,
        audio_in: Arc::new(CpalMic::new()),
        audio_out: Arc::new(CpalSpeaker::new()),
        vad,
        providers,
        runtime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_name_id_and_path_and_nothing_is_created() {
        let d = tempfile::tempdir().unwrap();
        let m = missing_models("default", d.path());
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["whisper-small", "silero-vad-v5", "phonon-31m"]);
        assert_eq!(m[0].path, d.path().join("whisper-small.bin"));
        assert!(
            m[0].notice().contains("whisper-small") && m[0].notice().contains("whisper-small.bin")
        );
        assert_eq!(
            std::fs::read_dir(d.path()).unwrap().count(),
            0,
            "nothing downloaded or created"
        );
        std::fs::write(d.path().join("whisper-small.bin"), b"x").unwrap();
        std::fs::create_dir(d.path().join("phonon-31m")).unwrap();
        let m = missing_models("default", d.path());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "silero-vad-v5");
        // The low-memory profile wants the small STT model.
        assert_eq!(missing_models("low-memory", d.path())[0].id, "whisper-base");
        assert!(missing_models("nope", d.path()).is_empty());
    }

    #[test]
    fn loader_reports_the_missing_stt_file_with_its_path() {
        let d = tempfile::tempdir().unwrap();
        let l = RealLoader::new(d.path().to_path_buf());
        let f = l.load_stt("whisper-small").err().expect("must not load");
        assert_eq!(f.token, "model_missing");
        assert_eq!(
            f.path.as_deref(),
            Some(d.path().join("whisper-small.bin").to_str().unwrap())
        );
        let f = l.load_tts("phonon-31m").err().expect("must not load");
        assert_eq!(f.token, "model_missing");
        assert_eq!(l.load_tts("kokoro").err().unwrap().token, "load_failed");
    }
}
