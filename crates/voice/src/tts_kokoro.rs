//! Kokoro-82M (ONNX) fallback TTS via ONNX Runtime. Feature `tts-kokoro`.
//!
//! ONNX Runtime is loaded dynamically (same setup as `vad_silero`): point `ORT_DYLIB_PATH`
//! at a libonnxruntime. Nothing is downloaded. Tensor names are read from the model at
//! load time and mapped to roles by element type and rank; mismatches fail with fixed
//! strings (names are never included).
//!
//! # Limits (read before relying on this)
//!
//! Kokoro consumes *phoneme token ids*, not text. The usual text-to-phoneme frontend is
//! espeak-ng (GPL-3.0), which the licence policy (D-002) does not allow as a dependency,
//! and no permissively-licensed G2P is wired in. This module therefore provides:
//! - model loading + I/O validation ([`KokoroCore::from_path`]);
//! - raw inference from token ids + a 256-float style vector ([`KokoroCore::infer`]);
//! - a [`Phonemizer`] trait so a frontend can be plugged in later;
//! - a `TtsCore` that errors with a fixed message until a `Phonemizer` is supplied.
//!
//! It does NOT turn English text into speech by itself. Voice style vectors (the Kokoro
//! `voices` files, which depend on token count) are likewise supplied by the caller.

use crate::tts_stream::{PcmIter, StreamingTts, TtsCore};
use crate::VoiceError;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;

pub const KOKORO_MODEL_ID: &str = "kokoro-82m";
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;
pub const STYLE_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Elem {
    F32,
    I64,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
struct IoDesc {
    name: String,
    elem: Elem,
    dims: Option<Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq)]
struct IoMap {
    tokens_in: String,
    style_in: String,
    speed_in: String,
    audio_out: String,
    /// The speed input is a rank-0 scalar (otherwise rank 1, shape `[1]`).
    speed_scalar: bool,
}

fn backend(what: &str) -> VoiceError {
    VoiceError::Backend(format!("kokoro: {what}"))
}

fn rank(d: &IoDesc) -> Option<usize> {
    d.dims.as_ref().map(Vec::len)
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

/// Map inputs/outputs to roles: tokens = i64 `[1, N]`, style = f32 `[1, 256]`,
/// speed = f32 scalar or `[1]`, audio = the single f32 output.
fn map_io(inputs: &[IoDesc], outputs: &[IoDesc]) -> Result<IoMap, VoiceError> {
    if inputs.len() != 3 || outputs.len() != 1 {
        return Err(backend("unexpected model input/output count"));
    }
    let tokens = pick(
        inputs,
        |d| d.elem == Elem::I64 && rank(d) == Some(2),
        "model has no unique token input",
    )?;
    let style = pick(
        inputs,
        |d| {
            d.elem == Elem::F32
                && d.dims
                    .as_ref()
                    .is_some_and(|v| v.len() == 2 && (v[1] < 0 || v[1] == STYLE_LEN as i64))
        },
        "model has no unique style input",
    )?;
    let speed = pick(
        inputs,
        |d| d.elem == Elem::F32 && matches!(rank(d), Some(0 | 1)),
        "model has no unique speed input",
    )?;
    let audio = pick(
        outputs,
        |d| d.elem == Elem::F32 && d.dims.is_some(),
        "model has no audio output",
    )?;
    Ok(IoMap {
        tokens_in: tokens.name.clone(),
        style_in: style.name.clone(),
        speed_in: speed.name.clone(),
        audio_out: audio.name.clone(),
        speed_scalar: rank(speed) == Some(0),
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

/// Text -> Kokoro phoneme token ids (including the pad/boundary tokens the model expects).
/// Must not log or embed the text in errors.
pub trait Phonemizer: Send + Sync + 'static {
    fn tokens(&self, sentence: &str) -> Result<Vec<i64>, VoiceError>;
}

pub struct KokoroCore {
    // `Session::run` needs `&mut` in this ort version.
    session: Mutex<Session>,
    io: IoMap,
    phonemizer: Option<Box<dyn Phonemizer>>,
    style: Option<Vec<f32>>,
    speed: f32,
}

impl KokoroCore {
    /// Load the model from `path`. Missing file -> `ModelMissing`.
    pub fn from_path(path: &Path) -> Result<Self, VoiceError> {
        if !path.is_file() {
            return Err(VoiceError::ModelMissing {
                path: path.to_path_buf(),
            });
        }
        let path = path.to_path_buf();
        // load-dynamic panics when the shared library cannot be found; convert to an error.
        let session = std::panic::catch_unwind(move || {
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
            session: Mutex::new(session),
            io,
            phonemizer: None,
            style: None,
            speed: 1.0,
        })
    }

    /// Load via the registry: `<models_dir>/kokoro-82m.onnx`.
    pub fn from_registry(
        registry: &crate::registry::Registry,
        models_dir: &Path,
    ) -> Result<Self, VoiceError> {
        Self::from_path(&registry.model_path(KOKORO_MODEL_ID, false, models_dir, "onnx")?)
    }

    /// Plug in a text frontend and a 256-float style vector so `TtsCore` can work.
    pub fn with_frontend(mut self, p: Box<dyn Phonemizer>, style: Vec<f32>) -> Self {
        self.phonemizer = Some(p);
        self.style = Some(style);
        self
    }

    /// Run the model on phoneme token ids. Returns mono f32 PCM at 24 kHz.
    pub fn infer(&self, tokens: &[i64], style: &[f32], speed: f32) -> Result<Vec<f32>, VoiceError> {
        if tokens.is_empty() {
            return Err(VoiceError::InvalidAudio("no tokens".into()));
        }
        if style.len() != STYLE_LEN {
            return Err(VoiceError::InvalidAudio(format!(
                "style vector must have {STYLE_LEN} values"
            )));
        }
        let tok_t = Tensor::from_array(([1usize, tokens.len()], tokens.to_vec()))
            .map_err(|_| backend("tensor"))?;
        let style_t = Tensor::from_array(([1usize, STYLE_LEN], style.to_vec()))
            .map_err(|_| backend("tensor"))?;
        let speed_t = if self.io.speed_scalar {
            Tensor::from_array((Vec::<usize>::new(), vec![speed]))
        } else {
            Tensor::from_array(([1usize], vec![speed]))
        }
        .map_err(|_| backend("tensor"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| backend("session poisoned"))?;
        let out = session
            .run(ort::inputs![
                self.io.tokens_in.as_str() => tok_t,
                self.io.style_in.as_str() => style_t,
                self.io.speed_in.as_str() => speed_t
            ])
            .map_err(|_| backend("inference failed"))?;
        let (_, audio) = out[self.io.audio_out.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|_| backend("bad output"))?;
        Ok(audio.to_vec())
    }
}

impl TtsCore for KokoroCore {
    fn sample_rate(&self) -> u32 {
        KOKORO_SAMPLE_RATE
    }

    fn synth(&self, sentence: &str, _voice: Option<&str>) -> Result<PcmIter, VoiceError> {
        let (Some(p), Some(style)) = (&self.phonemizer, &self.style) else {
            return Err(backend("text frontend (phonemiser) not available"));
        };
        let tokens = p.tokens(sentence)?;
        let pcm = self.infer(&tokens, style, self.speed)?;
        Ok(Box::new(std::iter::once(Ok(pcm))))
    }
}

pub type KokoroTts = StreamingTts<KokoroCore>;

pub fn into_provider(core: KokoroCore) -> KokoroTts {
    StreamingTts::new(KOKORO_MODEL_ID, core)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: &str, elem: Elem, dims: &[i64]) -> IoDesc {
        IoDesc {
            name: name.into(),
            elem,
            dims: Some(dims.to_vec()),
        }
    }

    fn ins() -> Vec<IoDesc> {
        vec![
            d("a", Elem::I64, &[1, -1]),
            d("b", Elem::F32, &[1, 256]),
            d("c", Elem::F32, &[1]),
        ]
    }

    fn outs() -> Vec<IoDesc> {
        vec![d("o", Elem::F32, &[-1])]
    }

    #[test]
    fn maps_roles_regardless_of_names_and_order() {
        let m = map_io(&ins(), &outs()).unwrap();
        assert_eq!(m.tokens_in, "a");
        assert_eq!(m.style_in, "b");
        assert_eq!(m.speed_in, "c");
        assert!(!m.speed_scalar);
        let mut i = ins();
        i.reverse();
        assert_eq!(map_io(&i, &outs()).unwrap(), m);
        i[0] = d("c", Elem::F32, &[]);
        assert!(map_io(&i, &outs()).unwrap().speed_scalar);
    }

    #[test]
    fn rejects_mismatched_models() {
        assert!(map_io(&ins()[..2], &outs()).is_err());
        let mut i = ins();
        i[1] = d("b", Elem::F32, &[1, 128]); // wrong style size
        assert!(map_io(&i, &outs()).is_err());
        let mut i = ins();
        i[0] = d("a", Elem::F32, &[1, -1]); // tokens must be i64
        assert!(map_io(&i, &outs()).is_err());
        let mut o = outs();
        o[0].dims = None;
        assert!(map_io(&ins(), &o).is_err());
        let mut o = outs();
        o.push(d("x", Elem::F32, &[1]));
        assert!(map_io(&ins(), &o).is_err());
    }

    #[test]
    fn mapping_errors_do_not_leak_names() {
        let mut i = ins();
        i[0] = d("secret_name", Elem::Other, &[1, -1]);
        let e = map_io(&i, &outs()).unwrap_err().to_string();
        assert!(!e.contains("secret_name"));
    }

    #[test]
    fn missing_model_is_model_missing() {
        let p = Path::new("/nonexistent/kokoro-82m.onnx");
        assert!(matches!(
            KokoroCore::from_path(p),
            Err(VoiceError::ModelMissing { path }) if path == p
        ));
    }

    /// Runs only when the model exists (`KOKORO_MODEL`, or `$HEY_BUDDY_MODELS_DIR/kokoro-82m.onnx`)
    /// and `ORT_DYLIB_PATH` is set. Any other load error fails (L-007). Runs raw inference with
    /// made-up token ids and a zero style (garbage audio, but proves the graph executes);
    /// text -> speech is not possible without a phonemiser (see module docs).
    #[test]
    fn raw_inference_when_model_present() {
        let path = std::env::var_os("KOKORO_MODEL")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HEY_BUDDY_MODELS_DIR")
                    .map(|d| Path::new(&d).join("kokoro-82m.onnx"))
            });
        let Some(p) = path else {
            eprintln!("SKIP kokoro integration test: KOKORO_MODEL / HEY_BUDDY_MODELS_DIR unset");
            return;
        };
        if !p.is_file() {
            eprintln!("SKIP kokoro integration test: model file missing");
            return;
        }
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            eprintln!("SKIP kokoro integration test: ORT_DYLIB_PATH not set");
            return;
        }
        let core = match KokoroCore::from_path(&p) {
            Ok(c) => c,
            Err(VoiceError::ModelMissing { .. }) => {
                eprintln!("SKIP kokoro integration test: model file missing");
                return;
            }
            Err(e) => panic!("kokoro load failed: {e}"),
        };
        let pcm = core
            .infer(&[0, 50, 83, 54, 0], &[0.0; STYLE_LEN], 1.0)
            .unwrap();
        assert!(!pcm.is_empty() && pcm.iter().all(|s| s.is_finite()));
        // Without a frontend, text synthesis fails with the fixed message.
        assert!(core.synth("hi", None).is_err());
    }
}
