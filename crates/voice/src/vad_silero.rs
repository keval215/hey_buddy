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

/// Element type of a model input/output, reduced to what role mapping needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Elem {
    F32,
    I64,
    Other,
}

/// What we know about one model input/output, read from the session at load time.
#[derive(Debug, Clone, PartialEq)]
struct IoDesc {
    name: String,
    elem: Elem,
    /// `None` when the value is not a tensor.
    dims: Option<Vec<i64>>,
}

/// Tensor names for each role, discovered from the model rather than hardcoded.
#[derive(Debug, Clone, PartialEq)]
struct IoMap {
    audio_in: String,
    state_in: String,
    sr_in: String,
    prob_out: String,
    state_out: String,
    /// The sample-rate input is a rank-0 scalar (otherwise rank 1, shape `[1]`).
    sr_scalar: bool,
}

fn rank(d: &IoDesc) -> Option<usize> {
    d.dims.as_ref().map(Vec::len)
}

fn is_state(d: &IoDesc) -> bool {
    // [2, batch, 128]; static dims (>= 0) must match.
    d.elem == Elem::F32
        && d.dims
            .as_ref()
            .is_some_and(|v| v.len() == 3 && (v[0] < 0 || v[0] == 2) && (v[2] < 0 || v[2] == 128))
}

fn is_audio_or_prob(d: &IoDesc) -> bool {
    d.elem == Elem::F32 && rank(d) == Some(2)
}

fn pick<'a>(
    items: &'a [IoDesc],
    pred: impl Fn(&IoDesc) -> bool,
    err: &str,
) -> Result<&'a IoDesc, VoiceError> {
    let mut it = items.iter().filter(|d| pred(d));
    match (it.next(), it.next()) {
        (Some(d), None) => Ok(d),
        _ => Err(backend(err)),
    }
}

/// Map model inputs/outputs to roles by element type and rank, and validate them.
/// Errors are fixed strings: tensor names from the model are never included.
fn map_io(inputs: &[IoDesc], outputs: &[IoDesc]) -> Result<IoMap, VoiceError> {
    if inputs.len() != 3 || outputs.len() != 2 {
        return Err(backend("unexpected model input/output count"));
    }
    let audio = pick(inputs, is_audio_or_prob, "model has no unique audio input")?;
    let state_in = pick(inputs, is_state, "model has no unique state input")?;
    let sr = pick(
        inputs,
        |d| d.elem == Elem::I64 && matches!(rank(d), Some(0 | 1)),
        "model has no unique sample-rate input",
    )?;
    let prob = pick(
        outputs,
        is_audio_or_prob,
        "model has no unique probability output",
    )?;
    let state_out = pick(outputs, is_state, "model has no unique state output")?;
    // Defensive: roles must resolve to distinct tensors.
    if audio.name == state_in.name || audio.name == sr.name || state_in.name == sr.name {
        return Err(backend("ambiguous model inputs"));
    }
    Ok(IoMap {
        audio_in: audio.name.clone(),
        state_in: state_in.name.clone(),
        sr_in: sr.name.clone(),
        prob_out: prob.name.clone(),
        state_out: state_out.name.clone(),
        sr_scalar: rank(sr) == Some(0),
    })
}

fn describe(o: &ort::value::Outlet) -> IoDesc {
    use ort::value::TensorElementType as T;
    let ty = o.dtype();
    let elem = match ty.tensor_type() {
        Some(T::Float32) => Elem::F32,
        Some(T::Int64) => Elem::I64,
        _ => Elem::Other,
    };
    IoDesc {
        name: o.name().to_string(),
        elem,
        dims: ty.tensor_shape().map(|s| s.to_vec()),
    }
}

pub struct SileroVad {
    session: Session,
    io: IoMap,
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
        let ins: Vec<IoDesc> = session.inputs().iter().map(describe).collect();
        let outs: Vec<IoDesc> = session.outputs().iter().map(describe).collect();
        let io = map_io(&ins, &outs)?;
        Ok(Self {
            session,
            io,
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT],
        })
    }

    /// Load via the registry (default id `silero-vad-v5`), file `<models_dir>/<id>.onnx`.
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
        let sr_t = if self.io.sr_scalar {
            Tensor::from_array((Vec::<usize>::new(), vec![16_000i64]))
        } else {
            Tensor::from_array(([1usize], vec![16_000i64]))
        }
        .map_err(|_| backend("tensor"))?;
        let out = self
            .session
            .run(ort::inputs![
                self.io.audio_in.as_str() => input_t,
                self.io.state_in.as_str() => state_t,
                self.io.sr_in.as_str() => sr_t
            ])
            .map_err(|_| backend("inference failed"))?;
        let (_, prob) = out[self.io.prob_out.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|_| backend("bad output"))?;
        let p = *prob.first().ok_or_else(|| backend("empty output"))?;
        let (_, st) = out[self.io.state_out.as_str()]
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
        let p = Path::new("/nonexistent/silero-vad-v5.onnx");
        assert!(matches!(
            SileroVad::from_path(p),
            Err(VoiceError::ModelMissing { path }) if path == p
        ));
    }

    fn d(name: &str, elem: Elem, dims: &[i64]) -> IoDesc {
        IoDesc {
            name: name.into(),
            elem,
            dims: Some(dims.to_vec()),
        }
    }

    fn v5_inputs() -> Vec<IoDesc> {
        vec![
            d("a", Elem::F32, &[-1, -1]),
            d("b", Elem::I64, &[]),
            d("c", Elem::F32, &[2, -1, 128]),
        ]
    }

    fn v5_outputs() -> Vec<IoDesc> {
        vec![
            d("p", Elem::F32, &[-1, 1]),
            d("s", Elem::F32, &[2, -1, 128]),
        ]
    }

    #[test]
    fn maps_roles_regardless_of_names_and_order() {
        let m = map_io(&v5_inputs(), &v5_outputs()).unwrap();
        assert_eq!(
            m,
            IoMap {
                audio_in: "a".into(),
                state_in: "c".into(),
                sr_in: "b".into(),
                prob_out: "p".into(),
                state_out: "s".into(),
                sr_scalar: true,
            }
        );
        let mut ins = v5_inputs();
        ins.reverse();
        let mut outs = v5_outputs();
        outs.reverse();
        assert_eq!(map_io(&ins, &outs).unwrap(), m);
        // rank-1 sample-rate input is accepted and flagged.
        ins[1] = d("b", Elem::I64, &[1]);
        assert!(!map_io(&ins, &outs).unwrap().sr_scalar);
    }

    #[test]
    fn rejects_mismatched_models() {
        let ok_in = v5_inputs();
        let ok_out = v5_outputs();
        // wrong counts
        assert!(map_io(&ok_in[..2], &ok_out).is_err());
        assert!(map_io(&ok_in, &ok_out[..1]).is_err());
        // wrong element type for sample rate
        let mut ins = ok_in.clone();
        ins[1] = d("b", Elem::F32, &[]);
        assert!(map_io(&ins, &ok_out).is_err());
        // state with wrong static size
        let mut ins = ok_in.clone();
        ins[2] = d("c", Elem::F32, &[2, -1, 64]);
        assert!(map_io(&ins, &ok_out).is_err());
        // non-tensor output
        let mut outs = ok_out.clone();
        outs[0].dims = None;
        assert!(map_io(&ok_in, &outs).is_err());
        // two audio-like inputs -> ambiguous
        let mut ins = ok_in.clone();
        ins[2] = d("c", Elem::F32, &[1, 512]);
        assert!(map_io(&ins, &ok_out).is_err());
    }

    #[test]
    fn mapping_errors_do_not_leak_names() {
        let mut ins = v5_inputs();
        ins[1] = d("secret_name", Elem::Other, &[]);
        let e = map_io(&ins, &v5_outputs()).unwrap_err().to_string();
        assert!(!e.contains("secret_name"));
    }

    /// Runs only when the model file exists (SILERO_VAD_MODEL, or
    /// `$HEY_BUDDY_MODELS_DIR/silero-vad-v5.onnx`) AND libonnxruntime is loadable.
    #[test]
    fn silence_is_not_speech_when_model_present() {
        let path = std::env::var_os("SILERO_VAD_MODEL")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HEY_BUDDY_MODELS_DIR")
                    .map(|d| Path::new(&d).join("silero-vad-v5.onnx"))
            })
            .filter(|p| p.is_file());
        let Some(p) = path else {
            eprintln!(
                "SKIP silero integration test: model not found \
                 (set SILERO_VAD_MODEL or HEY_BUDDY_MODELS_DIR, plus ORT_DYLIB_PATH)"
            );
            return;
        };
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            eprintln!("SKIP silero integration test: ORT_DYLIB_PATH not set");
            return;
        }
        let mut vad = SileroVad::from_path(&p).expect("load");
        let p = vad.speech_prob(&[0.0; WINDOW]).unwrap();
        assert!(p < 0.5);
        assert!(vad.speech_prob(&[0.0; 10]).is_err());
    }
}
