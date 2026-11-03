//! Silero VAD (v5 ONNX) via ONNX Runtime. Feature `vad-silero`.
//!
//! ONNX Runtime is loaded dynamically (`load-dynamic`): nothing is downloaded at build
//! time. Point `ORT_DYLIB_PATH` at a libonnxruntime. If it is missing, construction
//! fails with `VoiceError::Backend` (fail closed).

use crate::{Vad, VoiceError};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

const WINDOW: usize = 512;
const CONTEXT: usize = 64;
const STATE_LEN: usize = 2 * 128;

pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    context: Vec<f32>,
}

fn backend(what: &str) -> VoiceError {
    // Fixed text only; ort error details can be logged by callers at debug level if needed.
    VoiceError::Backend(format!("silero vad: {what}"))
}

impl SileroVad {
    /// Load the model from `path`. Missing file -> `ModelMissing`.
    pub fn from_path(path: &Path) -> Result<Self, VoiceError> {
        if !path.is_file() {
            return Err(VoiceError::ModelMissing {
                path: path.to_path_buf(),
            });
        }
        // load-dynamic panics when the shared library cannot be found; convert to an error.
        let path = path.to_path_buf();
        let session = std::panic::catch_unwind(move || {
            // Telemetry off: nothing leaves the device.
            ort::environment::init()
                .with_telemetry(false)
                .build()
                .and_then(|env| Session::builder(&env))
                .and_then(|mut b| b.commit_from_file(&path))
                .map_err(|_| ())
        })
        .map_err(|_| backend("onnxruntime library unavailable"))?
        .map_err(|_| backend("cannot load model"))?;
        Ok(Self {
            session,
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT],
        })
    }

    /// Load via the registry (default id `silero-vad`), file `<models_dir>/<id>.onnx`.
    pub fn from_registry(
        registry: &crate::registry::Registry,
        id: &str,
        models_dir: &Path,
    ) -> Result<Self, VoiceError> {
        Self::from_path(&registry.model_path(id, false, models_dir, "onnx")?)
    }
}

impl Vad for SileroVad {
    fn window_len(&self) -> usize {
        WINDOW
    }

    fn speech_prob(&mut self, window: &[f32]) -> Result<f32, VoiceError> {
        if window.len() != WINDOW {
            return Err(VoiceError::InvalidAudio(format!(
                "vad window must be {WINDOW} samples, got {}",
                window.len()
            )));
        }
        let mut input = Vec::with_capacity(CONTEXT + WINDOW);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(window);
        let input_t = Tensor::from_array(([1usize, CONTEXT + WINDOW], input.clone()))
            .map_err(|_| backend("tensor"))?;
        let state_t = Tensor::from_array(([2usize, 1, 128], self.state.clone()))
            .map_err(|_| backend("tensor"))?;
        let sr_t =
            Tensor::from_array(([1usize], vec![16_000i64])).map_err(|_| backend("tensor"))?;
        let out = self
            .session
            .run(ort::inputs!["input" => input_t, "state" => state_t, "sr" => sr_t])
            .map_err(|_| backend("inference failed"))?;
        let (_, prob) = out["output"]
            .try_extract_tensor::<f32>()
            .map_err(|_| backend("bad output"))?;
        let p = *prob.first().ok_or_else(|| backend("empty output"))?;
        let (_, st) = out["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|_| backend("bad state"))?;
        if st.len() != STATE_LEN {
            return Err(backend("bad state size"));
        }
        self.state.copy_from_slice(st);
        self.context
            .copy_from_slice(&input[input.len() - CONTEXT..]);
        Ok(p.clamp(0.0, 1.0))
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|x| *x = 0.0);
        self.context.iter_mut().for_each(|x| *x = 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_is_model_missing() {
        let p = Path::new("/nonexistent/silero-vad.onnx");
        assert!(matches!(
            SileroVad::from_path(p),
            Err(VoiceError::ModelMissing { path }) if path == p
        ));
    }

    /// Runs only when SILERO_VAD_MODEL points at a model AND libonnxruntime is loadable.
    #[test]
    fn silence_is_not_speech_when_model_present() {
        let Some(p) = std::env::var_os("SILERO_VAD_MODEL") else {
            eprintln!("SKIP silero test: set SILERO_VAD_MODEL (and ORT_DYLIB_PATH) to run");
            return;
        };
        let mut vad = SileroVad::from_path(Path::new(&p)).expect("load");
        let p = vad.speech_prob(&[0.0; WINDOW]).unwrap();
        assert!(p < 0.5);
        assert!(vad.speech_prob(&[0.0; 10]).is_err());
    }
}
