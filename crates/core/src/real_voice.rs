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
use voice::{tts_phonon, AudioInput, AudioOutput, SttProvider, TtsProvider, Vad, VoiceError};

use crate::settings::builtin_registry;
use crate::turn::VadFactory;

/// Environment variable that overrides the models directory.
pub const MODELS_DIR_ENV: &str = "HEY_BUDDY_MODELS_DIR";

pub struct RealVoice {
    pub audio_in: Arc<dyn AudioInput>,
    pub audio_out: Arc<dyn AudioOutput>,
    pub vad: VadFactory,
    pub stt: Arc<dyn SttProvider>,
    pub tts: Arc<dyn TtsProvider>,
}

/// `HEY_BUDDY_MODELS_DIR` if set and non-empty, else `<app_data_dir>/models`.
pub fn models_dir(app_data_dir: &Path) -> PathBuf {
    match std::env::var_os(MODELS_DIR_ENV) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => app_data_dir.join("models"),
    }
}

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

/// Load the adapters for `profile` from `dir`. Slow (loads the models): call off the UI
/// thread. The first failure is returned (`ModelMissing` names the path).
pub fn build(profile: &str, dir: &Path) -> Result<RealVoice, VoiceError> {
    let reg = builtin_registry();
    let p = reg
        .profile(profile)
        .ok_or_else(|| VoiceError::Backend("unknown profile".into()))?;
    let stt = WhisperStt::from_registry(reg, &p.stt.model, dir)?;
    let tts = tts_phonon::from_registry(reg, dir)?;
    let vad_model = vad_id(reg);
    // Load once now so a missing file or onnxruntime library is reported at startup; each
    // turn gets a fresh detector (it keeps per-utterance state).
    drop(SileroVad::from_registry(reg, &vad_model, dir)?);
    let dir = dir.to_path_buf();
    let vad: VadFactory = Arc::new(move || {
        Ok(Box::new(SileroVad::from_registry(
            builtin_registry(),
            &vad_model,
            &dir,
        )?) as Box<dyn Vad>)
    });
    Ok(RealVoice {
        audio_in: Arc::new(CpalMic::new()),
        audio_out: Arc::new(CpalSpeaker::new()),
        vad,
        stt: Arc::new(stt),
        tts: Arc::new(tts),
    })
}

/// The whole engine for the real adapters: platform firewall, `deny`, the voice adapters
/// for `profile` from `dir`, and `providers`. Slow (loads models): call off the UI thread.
pub fn engine_deps(
    profile: &str,
    dir: &Path,
    deny: Arc<capture::DenyList>,
    providers: Arc<dyn crate::turn::ProviderSource>,
    runtime: tokio::runtime::Handle,
) -> Result<crate::turn::EngineDeps, VoiceError> {
    let v = build(profile, dir)?;
    Ok(crate::turn::EngineDeps {
        firewall: crate::setup::platform_firewall(),
        deny,
        audio_in: v.audio_in,
        audio_out: v.audio_out,
        vad: v.vad,
        stt: v.stt,
        tts: v.tts,
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
    fn build_reports_the_missing_stt_file() {
        let d = tempfile::tempdir().unwrap();
        match build("default", d.path()) {
            Err(VoiceError::ModelMissing { path }) => {
                assert_eq!(path, d.path().join("whisper-small.bin"))
            }
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("must not load anything"),
        }
    }
}
