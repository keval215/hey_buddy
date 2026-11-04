//! The first-run wizard as a pure state machine (S3-4). No I/O, no clock, no platform calls:
//! [`transition`] maps `(state, event)` to the next state and says whether the safe settings
//! must be (re)asserted. Probes (machine, microphone, Ollama) live in [`super::probe`] and only
//! run when the user clicks; the machine itself never starts one.
//!
//! Safety rule (D-008): nothing here can turn a protection OFF. The only settings change a
//! transition can ask for is [`safe_defaults_patch`], which contains safe values only, and it is
//! asked for only when the user confirms the safety step. Skipping or abandoning the wizard
//! changes no setting at all.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::{Settings, SettingsPatch, VisionMode};
use crate::settings::model_id_issue;

/// The wizard's steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum WizardStep {
    /// What is local, what could go to the cloud, nothing sent without a preview.
    Welcome,
    MachineCheck,
    Microphone,
    Ollama,
    Models,
    Hotkey,
    /// Confirm the safe settings. Cannot be skipped; cannot turn anything off.
    SafetyDefaults,
    Done,
}

impl WizardStep {
    pub const ALL: [WizardStep; 8] = [
        Self::Welcome,
        Self::MachineCheck,
        Self::Microphone,
        Self::Ollama,
        Self::Models,
        Self::Hotkey,
        Self::SafetyDefaults,
        Self::Done,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<Self> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub fn prev(self) -> Option<Self> {
        self.index().checked_sub(1).map(|i| Self::ALL[i])
    }

    /// Skip is allowed everywhere except the safety confirmation (and the final screen, which
    /// has nothing to skip).
    pub fn can_skip(self) -> bool {
        !matches!(self, Self::SafetyDefaults | Self::Done)
    }
}

/// The hardware profile the machine check recommends. The names are the registry's profile
/// names, so the UI can send them straight to `update_settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum WizardProfile {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "low-memory")]
    LowMemory,
}

impl WizardProfile {
    pub fn registry_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::LowMemory => "low-memory",
        }
    }
}

/// What the machine check found. `None` = unknown (the probe could not tell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MachineInfo {
    /// Installed RAM in MiB.
    pub ram_mb: Option<u32>,
    pub gpu_name: Option<String>,
    /// Dedicated video memory of the best GPU in MiB.
    pub vram_mb: Option<u32>,
}

impl MachineInfo {
    pub const UNKNOWN: MachineInfo = MachineInfo {
        ram_mb: None,
        gpu_name: None,
        vram_mb: None,
    };

    /// The GPU name made safe to store and show: control characters dropped, capped.
    pub fn sanitized(mut self) -> Self {
        self.gpu_name = self.gpu_name.map(|n| {
            let clean: String = n
                .chars()
                .filter(|c| !crate::models_path::is_hostile_char(*c))
                .take(MAX_GPU_NAME)
                .collect();
            clean.trim().to_string()
        });
        if self.gpu_name.as_deref() == Some("") {
            self.gpu_name = None;
        }
        self
    }

    fn valid(&self) -> bool {
        self.ram_mb.is_none_or(|r| r <= MAX_RAM_MB)
            && self.vram_mb.is_none_or(|v| v <= MAX_VRAM_MB)
            && self.gpu_name.as_deref().is_none_or(|n| {
                n.chars().count() <= MAX_GPU_NAME
                    && !n.is_empty()
                    && !n.chars().any(crate::models_path::is_hostile_char)
            })
    }
}

const MAX_GPU_NAME: usize = 128;
const MAX_RAM_MB: u32 = 16 * 1024 * 1024;
const MAX_VRAM_MB: u32 = 1024 * 1024;
const MAX_OLLAMA_MODELS: usize = 8;

/// RAM below this (MiB) recommends `low-memory`.
pub const MIN_RAM_MB: u32 = 12 * 1024;
/// Dedicated video memory below this (MiB) recommends `low-memory`. A 4 GB card (the registry's
/// `default` profile target) reports a little under 4096 MiB, so the bar is 3.5 GiB.
pub const MIN_VRAM_MB: u32 = 3584;

/// The recommendation table. Anything unknown is `low-memory` (the safe, small choice).
pub fn recommend_profile(m: &MachineInfo) -> WizardProfile {
    match (m.ram_mb, m.vram_mb) {
        (Some(ram), Some(vram)) if ram >= MIN_RAM_MB && vram >= MIN_VRAM_MB => {
            WizardProfile::Default
        }
        _ => WizardProfile::LowMemory,
    }
}

/// One recommended Ollama model tag and whether Ollama already has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OllamaModelDto {
    pub tag: String,
    pub present: bool,
}

/// What the Ollama check found. `running: false` covers "not installed" and "not started"
/// (nothing answered on loopback): the wizard cannot tell them apart and says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OllamaInfo {
    pub running: bool,
    pub models: Vec<OllamaModelDto>,
}

impl OllamaInfo {
    fn valid(&self) -> bool {
        self.models.len() <= MAX_OLLAMA_MODELS
            && self.models.iter().all(|m| model_id_issue(&m.tag).is_ok())
            && (self.running || self.models.iter().all(|m| !m.present))
    }
}

/// Everything the wizard remembers. Persisted in `settings.json`; checked on load (see
/// [`WizardState::from_value`]): anything unexpected becomes [`WizardState::default`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WizardState {
    pub step: WizardStep,
    /// The user clicked Finish. Only ever true together with `step == Done`.
    pub completed: bool,
    /// Steps the user skipped (never `SafetyDefaults` or `Done`), in no particular order.
    pub skipped_steps: Vec<WizardStep>,
    pub recommended_profile: Option<WizardProfile>,
    pub machine: Option<MachineInfo>,
    pub ollama: Option<OllamaInfo>,
    /// `None` = not tested.
    pub mic_ok: Option<bool>,
    pub hotkey_ok: Option<bool>,
}

impl Default for WizardState {
    /// The fail-closed state: start over from the Welcome screen, not completed.
    fn default() -> Self {
        Self {
            step: WizardStep::Welcome,
            completed: false,
            skipped_steps: Vec::new(),
            recommended_profile: None,
            machine: None,
            ollama: None,
            mic_ok: None,
            hotkey_ok: None,
        }
    }
}

impl WizardState {
    /// Internal consistency. A state that fails this is never used.
    pub fn is_valid(&self) -> bool {
        let mut seen = Vec::new();
        for s in &self.skipped_steps {
            // Only steps already passed can have been skipped.
            if !s.can_skip() || seen.contains(s) || s.index() >= self.step.index() {
                return false;
            }
            seen.push(*s);
        }
        // Done with `completed: false` is legitimate (the final screen before Finish is clicked).
        if self.completed && self.step != WizardStep::Done {
            return false;
        }
        if let Some(m) = &self.machine {
            // The recommendation is always derived from the machine, never stored on its own.
            if !m.valid() || self.recommended_profile != Some(recommend_profile(m)) {
                return false;
            }
        } else if self.recommended_profile.is_some() {
            return false;
        }
        self.ollama.as_ref().is_none_or(OllamaInfo::valid)
    }

    /// Parse a stored value; unknown steps, wrong shapes and inconsistent states give the
    /// fail-closed default (Welcome, not completed).
    pub fn from_value(v: serde_json::Value) -> Self {
        Self::try_from_value(v).unwrap_or_default()
    }

    /// Like [`from_value`](Self::from_value), but `None` for anything not accepted.
    pub fn try_from_value(v: serde_json::Value) -> Option<Self> {
        serde_json::from_value::<WizardState>(v)
            .ok()
            .filter(WizardState::is_valid)
    }

    fn fresh_run() -> Self {
        Self::default()
    }

    fn mark_skipped(&mut self, s: WizardStep) {
        if !self.skipped_steps.contains(&s) {
            self.skipped_steps.push(s);
        }
    }

    fn with_step(mut self, s: WizardStep) -> Self {
        self.step = s;
        self
    }
}

/// What can happen to the wizard. The `*Result` events are produced by core after a click-driven
/// probe; the webview may only send the others (see `Buddy::wizard_event`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum WizardEvent {
    Next,
    Back,
    Skip,
    /// Forget this step's result so the check can be run again (runs nothing itself).
    Retry,
    MachineResult(MachineInfo),
    OllamaResult(OllamaInfo),
    MicResult(bool),
    /// The push-to-talk shortcut was accepted and registered (or not). Informational only.
    HotkeyResult(bool),
    /// Leave the finished wizard (from `Done` only).
    Finish,
    /// Start over ("Run setup again"). Changes no setting.
    Rerun,
}

impl WizardEvent {
    /// Events that carry a probe result and so may only come from core.
    pub fn is_probe_result(&self) -> bool {
        matches!(
            self,
            Self::MachineResult(_) | Self::OllamaResult(_) | Self::MicResult(_)
        )
    }
}

/// The outcome of one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub state: WizardState,
    /// The user confirmed the safety step: apply [`safe_defaults_patch`].
    pub apply_safe_defaults: bool,
}

/// The only settings change the wizard can make, and it only ever tightens (D-008): local-only
/// ON, observe mode ON, agent and agent mode OFF, vision Always -> Auto. Vision that is already
/// Off or Auto is left alone (the patch says nothing about it), so a user's stricter choice (Off,
/// or Off forced by `low-memory`) is never loosened. `current` is the effective settings.
/// The deny list is shown, never edited, by the wizard.
pub fn safe_defaults_patch(current: &Settings) -> SettingsPatch {
    SettingsPatch {
        local_only: Some(true),
        observe_mode: Some(true),
        vision_mode: (current.vision_mode == VisionMode::Always).then_some(VisionMode::Auto),
        agent_enabled: Some(false),
        agent_mode: Some(false),
        ..Default::default()
    }
}

fn stay(state: &WizardState) -> Transition {
    Transition {
        state: state.clone(),
        apply_safe_defaults: false,
    }
}

fn moved(state: WizardState) -> Transition {
    Transition {
        state,
        apply_safe_defaults: false,
    }
}

/// The pure transition function. Invalid events for the current step change nothing.
pub fn transition(state: &WizardState, ev: &WizardEvent) -> Transition {
    // Never continue from a state that is not consistent.
    if !state.is_valid() {
        return moved(WizardState::default());
    }
    use WizardStep as S;
    match ev {
        WizardEvent::Next => match state.step {
            S::Done => stay(state),
            S::SafetyDefaults => Transition {
                state: state.clone().with_step(S::Done),
                apply_safe_defaults: true,
            },
            s => {
                let mut st = state.clone();
                st.skipped_steps.retain(|x| *x != s);
                moved(st.with_step(s.next().unwrap_or(S::Done)))
            }
        },
        WizardEvent::Back => match (state.step.prev(), state.completed) {
            (Some(p), false) => {
                // Steps from here on are not "passed" any more: their skip marks go.
                let mut st = state.clone().with_step(p);
                st.skipped_steps.retain(|x| x.index() < p.index());
                moved(st)
            }
            _ => stay(state),
        },
        WizardEvent::Skip => {
            let s = state.step;
            if !s.can_skip() {
                return stay(state);
            }
            let mut st = state.clone();
            st.mark_skipped(s);
            moved(st.with_step(s.next().unwrap_or(S::Done)))
        }
        WizardEvent::Retry => {
            let mut st = state.clone();
            match state.step {
                S::MachineCheck => {
                    st.machine = None;
                    st.recommended_profile = None;
                }
                S::Microphone => st.mic_ok = None,
                S::Ollama => st.ollama = None,
                S::Hotkey => st.hotkey_ok = None,
                _ => return stay(state),
            }
            moved(st)
        }
        WizardEvent::MachineResult(m) if state.step == S::MachineCheck => {
            let m = m.clone().sanitized();
            if !m.valid() {
                return stay(state);
            }
            let mut st = state.clone();
            st.recommended_profile = Some(recommend_profile(&m));
            st.machine = Some(m);
            st.skipped_steps.retain(|x| *x != S::MachineCheck);
            moved(st)
        }
        WizardEvent::OllamaResult(o) if state.step == S::Ollama => {
            if !o.valid() {
                return stay(state);
            }
            let mut st = state.clone();
            st.ollama = Some(o.clone());
            st.skipped_steps.retain(|x| *x != S::Ollama);
            moved(st)
        }
        WizardEvent::MicResult(ok) if state.step == S::Microphone => {
            let mut st = state.clone();
            st.mic_ok = Some(*ok);
            st.skipped_steps.retain(|x| *x != S::Microphone);
            moved(st)
        }
        WizardEvent::HotkeyResult(ok) if state.step == S::Hotkey => {
            let mut st = state.clone();
            st.hotkey_ok = Some(*ok);
            st.skipped_steps.retain(|x| *x != S::Hotkey);
            moved(st)
        }
        // A result for a step the wizard is not on is stale or forged: dropped.
        WizardEvent::MachineResult(_)
        | WizardEvent::OllamaResult(_)
        | WizardEvent::MicResult(_)
        | WizardEvent::HotkeyResult(_) => stay(state),
        WizardEvent::Finish => {
            if state.step == S::Done {
                let mut st = state.clone();
                st.completed = true;
                moved(st)
            } else {
                stay(state)
            }
        }
        WizardEvent::Rerun => moved(WizardState::fresh_run()),
    }
}
