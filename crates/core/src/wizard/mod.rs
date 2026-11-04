//! First-run wizard (S3-4). See `docs/wizard.md`.
//!
//! - [`state`]: the pure state machine (`transition`), the safe-defaults patch and the
//!   recommendation table. No I/O.
//! - [`probe`]: machine / microphone / Ollama probes behind traits, run only on a click.
//! - `Buddy::wizard_*` (in `buddy.rs`) wires them to the settings file.

#[cfg(windows)]
mod machine_windows;
pub mod probe;
pub mod state;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use probe::{
    ollama_info, platform_machine_probe, AudioInputProbe, LoopbackOllamaProbe, MachineProbe,
    MicLevel, MicProbe, OllamaProbe, UnknownMachineProbe, WizardError, MIC_LISTEN, MIC_MIN_PEAK,
    MIC_TIMEOUT,
};
#[cfg(any(test, feature = "fakes"))]
pub use probe::{FakeMachineProbe, FakeMicProbe, FakeOllamaProbe};
pub use state::{
    recommend_profile, safe_defaults_patch, transition, MachineInfo, OllamaInfo, OllamaModelDto,
    Transition, WizardEvent, WizardProfile, WizardState, WizardStep, MIN_RAM_MB, MIN_VRAM_MB,
};

use voice::registry::Task;

use crate::settings::builtin_registry;

/// What the wizard window shows: the state plus the facts it needs from the active profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WizardView {
    pub state: WizardState,
    /// The profile in the settings now (the user may have overridden the recommendation).
    pub profile: String,
    /// Registry ids the Models step shows (the profile's STT, TTS, LLM, VLM and the VAD).
    pub model_ids: Vec<String>,
    /// Ollama tags the Ollama check looks for.
    pub ollama_tags: Vec<String>,
}

/// Result of the microphone check: a level and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MicProbeDto {
    /// 0.0 to 1.0.
    pub peak: f32,
    /// 0.0 to 1.0.
    pub rms: f32,
    /// A fixed token when the check could not run (`mic_unavailable`, `mic_timeout`, ...).
    pub error: Option<String>,
    pub view: WizardView,
}

/// Registry ids the Models step lists for `profile`.
pub fn profile_model_ids(profile: &str) -> Vec<String> {
    let reg = builtin_registry();
    let Some(p) = reg.profile(profile) else {
        return Vec::new();
    };
    let mut ids = vec![
        p.stt.model.clone(),
        p.tts.model.clone(),
        p.llm.model.clone(),
    ];
    if let Some(v) = &p.vlm {
        ids.push(v.model.clone());
    }
    if let Some(vad) = reg.default_for(Task::Vad) {
        ids.push(vad.id.clone());
    }
    ids
}
