//! Settings validation and persistence. Pure logic plus one JSON file in the app config dir.
//! Written atomically (temp file + rename). Never stores secrets (the API key lives in the OS
//! keychain, T11). Errors carry fixed text only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use voice::registry::Registry;

use crate::api::{
    IssueCode, ProviderChoice, Settings, SettingsField, SettingsIssue, SettingsPatch, VisionMode,
    DEFAULT_HOTKEY, DEFAULT_KILL_HOTKEY, DEFAULT_PROFILE,
};
use crate::models_path::validate_models_dir;
use crate::route::effective_vision_mode;
use crate::wizard::WizardState;

/// Fixed-text errors (no paths, no content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("unknown profile")]
    InvalidProfile,
    #[error("invalid hotkey")]
    InvalidHotkey,
    #[error("invalid model id")]
    InvalidModel,
    #[error("could not save settings")]
    Io,
}

/// Token surfaced in `Status.settings_error` when the file was unreadable.
pub const ERR_SETTINGS_CORRUPT: &str = "settings_corrupt";

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| {
        Registry::from_json_str(include_str!("../../../models/registry.json"))
            .expect("models/registry.json is validated by the voice crate tests")
    })
}

/// The model registry compiled into the binary.
pub fn builtin_registry() -> &'static Registry {
    registry()
}

/// Profile names the registry knows, sorted.
pub fn profile_names() -> Vec<String> {
    registry().profiles.keys().cloned().collect()
}

/// The vision mode that applies for `profile` (Off when it has no VLM).
pub fn effective_vision(mode: VisionMode, profile: &str) -> VisionMode {
    match registry().profile(profile) {
        Some(p) => effective_vision_mode(mode.into(), p).into(),
        None => VisionMode::Off,
    }
}

/// Longest accepted model id.
pub const MAX_MODEL_ID_LEN: usize = 80;

/// Anthropic model id: empty (cloud unavailable) or at most [`MAX_MODEL_ID_LEN`] chars of
/// `[A-Za-z0-9._:-]`. Returned trimmed.
pub fn validate_model_id(s: &str) -> Result<String, SettingsError> {
    model_id_issue(s).map_err(|_| SettingsError::InvalidModel)
}

/// Like [`validate_model_id`] with the reason.
pub fn model_id_issue(s: &str) -> Result<String, IssueCode> {
    let s = s.trim();
    if s.len() > MAX_MODEL_ID_LEN {
        return Err(IssueCode::TooLong);
    }
    if s.chars().any(crate::models_path::is_hostile_char) {
        return Err(IssueCode::ControlChars);
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(IssueCode::InvalidFormat);
    }
    Ok(s.to_string())
}

/// Validate and canonicalise a push-to-talk shortcut: at least one of Ctrl, Alt or Super (Cmd)
/// (optionally with Shift) plus exactly one key (A-Z, 0-9, F1-F24, Space), e.g.
/// `ctrl+alt+space` -> `Ctrl+Alt+Space`. A bare key or a Shift-only chord is rejected so the
/// hotkey never steals typing.
pub fn normalize_hotkey(s: &str) -> Result<String, SettingsError> {
    hotkey_issue(s).map_err(|_| SettingsError::InvalidHotkey)
}

/// Like [`normalize_hotkey`] with the reason: `NeedsModifier` for a chord without
/// Ctrl/Alt/Super, `TooLong`, `ControlChars`, otherwise `InvalidFormat`.
pub fn hotkey_issue(s: &str) -> Result<String, IssueCode> {
    const BAD: IssueCode = IssueCode::InvalidFormat;
    if s.len() > 64 {
        return Err(IssueCode::TooLong);
    }
    if s.chars().any(crate::models_path::is_hostile_char) {
        return Err(IssueCode::ControlChars);
    }
    let (mut ctrl, mut alt, mut shift, mut sup) = (false, false, false, false);
    let mut key: Option<String> = None;
    for part in s.split('+') {
        let p = part.trim();
        let slot = match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(&mut ctrl),
            "alt" | "option" => Some(&mut alt),
            "shift" => Some(&mut shift),
            "super" | "cmd" | "command" | "meta" | "win" => Some(&mut sup),
            _ => None,
        };
        match slot {
            Some(flag) => {
                if *flag {
                    return Err(BAD);
                }
                *flag = true;
            }
            None => {
                if key.is_some() {
                    return Err(BAD);
                }
                key = Some(normalize_key(p).ok_or(BAD)?);
            }
        }
    }
    let key = key.ok_or(BAD)?;
    if !(ctrl || alt || sup) {
        return Err(IssueCode::NeedsModifier);
    }
    let mut out = Vec::new();
    for (on, name) in [
        (ctrl, "Ctrl"),
        (alt, "Alt"),
        (shift, "Shift"),
        (sup, "Super"),
    ] {
        if on {
            out.push(name.to_string());
        }
    }
    out.push(key);
    Ok(out.join("+"))
}

fn normalize_key(p: &str) -> Option<String> {
    if p.eq_ignore_ascii_case("space") {
        return Some("Space".into());
    }
    if p.len() == 1 {
        let c = p.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some((c as char).to_ascii_uppercase().to_string());
        }
        return None;
    }
    let up = p.to_ascii_uppercase();
    let n = up.strip_prefix('F')?;
    if n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0') {
        let n: u8 = n.parse().ok()?;
        if (1..=24).contains(&n) {
            return Some(format!("F{n}"));
        }
    }
    None
}

/// What is written to disk: the user's choices only. `vision_mode` keeps the preference even
/// when `low-memory` currently forces Off, so switching back restores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    local_only: bool,
    vision_mode: VisionMode,
    profile: String,
    provider: ProviderChoice,
    hotkey: String,
    kill_hotkey: String,
    anthropic_model: String,
    models_dir: String,
    observe_mode: bool,
    agent_enabled: bool,
    agent_mode: bool,
    /// Local diagnostic log file (S3-6), default OFF; read at startup.
    diagnostic_logs: bool,
    /// First-run wizard progress (S3-4). Read leniently: a bad value becomes the fail-closed
    /// default (see [`SettingsStore::load`]).
    #[serde(deserialize_with = "lenient_wizard")]
    wizard: WizardState,
}

fn lenient_wizard<'de, D: serde::Deserializer<'de>>(d: D) -> Result<WizardState, D::Error> {
    Ok(WizardState::from_value(serde_json::Value::deserialize(d)?))
}

/// The file has a `wizard` entry the wizard module does not accept.
fn wizard_entry_invalid(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|mut v| v.get_mut("wizard").map(serde_json::Value::take))
        .is_some_and(|w| WizardState::try_from_value(w).is_none())
}

impl Default for Stored {
    fn default() -> Self {
        let d = Settings::default();
        Self {
            local_only: d.local_only,
            vision_mode: d.vision_mode,
            profile: DEFAULT_PROFILE.into(),
            provider: ProviderChoice::Ollama,
            hotkey: DEFAULT_HOTKEY.into(),
            kill_hotkey: DEFAULT_KILL_HOTKEY.into(),
            anthropic_model: String::new(),
            models_dir: String::new(),
            observe_mode: d.observe_mode,
            agent_enabled: d.agent_enabled,
            agent_mode: d.agent_mode,
            diagnostic_logs: d.diagnostic_logs,
            wizard: WizardState::default(),
        }
    }
}

fn issue(field: SettingsField, code: IssueCode) -> SettingsIssue {
    SettingsIssue { field, code }
}

impl Stored {
    /// Replace every invalid value by the safest one (default profile, default hotkey, no
    /// cloud model, default models folder) and say what was replaced. The rejected text is
    /// dropped here: it never reaches the stored settings or the file. `only` limits the
    /// check to the fields of a patch, so a folder that vanished since it was saved is not
    /// "fixed" by an unrelated save.
    fn sanitize(&self, only: Option<&SettingsPatch>) -> (Stored, Vec<SettingsIssue>) {
        let wants = |f: fn(&SettingsPatch) -> bool| only.is_none_or(f);
        let mut out = self.clone();
        let mut issues = Vec::new();
        if wants(|p| p.profile.is_some()) && registry().profile(&self.profile).is_none() {
            out.profile = DEFAULT_PROFILE.into();
            issues.push(issue(SettingsField::Profile, IssueCode::Unknown));
        }
        if wants(|p| p.hotkey.is_some()) {
            match hotkey_issue(&self.hotkey) {
                Ok(h) => out.hotkey = h,
                Err(code) => {
                    out.hotkey = DEFAULT_HOTKEY.into();
                    issues.push(issue(SettingsField::Hotkey, code));
                }
            }
        }
        if wants(|p| p.kill_hotkey.is_some()) {
            match crate::kill_hotkey::kill_hotkey_issue(&self.kill_hotkey, &out.hotkey) {
                Ok(h) => out.kill_hotkey = h,
                Err(code) => {
                    out.kill_hotkey = DEFAULT_KILL_HOTKEY.into();
                    issues.push(issue(SettingsField::KillHotkey, code));
                }
            }
        }
        // A push-to-talk change that lands on the kill chord is the one rejected (a kill chord
        // equal to the push-to-talk chord was already rejected above).
        if out.hotkey == out.kill_hotkey {
            out.hotkey = DEFAULT_HOTKEY.into();
            issues.push(issue(SettingsField::Hotkey, IssueCode::Duplicate));
        }
        if wants(|p| p.anthropic_model.is_some()) {
            match model_id_issue(&self.anthropic_model) {
                Ok(m) => out.anthropic_model = m,
                Err(code) => {
                    out.anthropic_model = String::new();
                    issues.push(issue(SettingsField::AnthropicModel, code));
                }
            }
        }
        if wants(|p| p.models_dir.is_some()) {
            match validate_models_dir(&self.models_dir) {
                Ok(d) => out.models_dir = d,
                Err(code) => {
                    out.models_dir = String::new();
                    issues.push(issue(SettingsField::ModelsDir, code));
                }
            }
        }
        // Routing voice to the agent needs the agent to be allowed.
        if !out.agent_enabled {
            out.agent_mode = false;
        }
        (out, issues)
    }
}

#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    stored: Stored,
    load_error: Option<&'static str>,
    /// Stored values replaced while loading (cleared by the next successful save).
    load_issues: Vec<SettingsIssue>,
}

impl SettingsStore {
    /// Load `path`. Missing file -> defaults. Unreadable, corrupt or invalid -> defaults and
    /// [`load_error`](Self::load_error) set; the bad file is left alone until the next save.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut store = Self {
            path,
            stored: Stored::default(),
            load_error: None,
            load_issues: Vec::new(),
        };
        match fs::read(&store.path) {
            Ok(bytes) => {
                // An unparseable file, or any invalid profile, hotkey or model id, means the
                // whole file is not trusted: defaults. Only a models folder that is gone (a
                // disconnected drive) is replaced on its own, with an issue.
                let parsed = serde_json::from_slice::<Stored>(&bytes).map_err(|_| ());
                match parsed {
                    Ok(s) => {
                        let (mut clean, issues) = s.sanitize(None);
                        // A wizard entry we do not understand means the file was edited or
                        // damaged: the wizard starts over AND the protections go back to their
                        // safe values (fail closed). Tighten only: vision Always becomes Auto,
                        // but Off stays Off. A `File`/`Corrupt` load issue tells the UI.
                        let mut issues = issues;
                        let reset = wizard_entry_invalid(&bytes);
                        if reset {
                            let d = Stored::default();
                            clean.local_only = d.local_only;
                            clean.observe_mode = d.observe_mode;
                            if clean.vision_mode == VisionMode::Always {
                                clean.vision_mode = VisionMode::Auto;
                            }
                            clean.agent_enabled = d.agent_enabled;
                            clean.agent_mode = d.agent_mode;
                            clean.diagnostic_logs = d.diagnostic_logs;
                        }
                        if issues.iter().any(|i| i.field != SettingsField::ModelsDir) {
                            store.load_error = Some(ERR_SETTINGS_CORRUPT);
                        } else {
                            store.stored = clean;
                            if reset {
                                issues.push(issue(SettingsField::File, IssueCode::Corrupt));
                            }
                            store.load_issues = issues;
                        }
                    }
                    Err(()) => store.load_error = Some(ERR_SETTINGS_CORRUPT),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => store.load_error = Some(ERR_SETTINGS_CORRUPT),
        }
        store
    }

    pub fn load_error(&self) -> Option<&'static str> {
        self.load_error
    }

    /// Values replaced while loading the file (e.g. a models folder that no longer exists).
    pub fn load_issues(&self) -> &[SettingsIssue] {
        &self.load_issues
    }

    /// The wizard's saved progress (the fail-closed default when none or unreadable).
    pub fn wizard(&self) -> WizardState {
        self.stored.wizard.clone()
    }

    /// Persist the wizard progress atomically. Nothing else in the file changes. An invalid
    /// state is refused.
    pub fn set_wizard(&mut self, w: WizardState) -> Result<(), SettingsError> {
        if !w.is_valid() {
            return Err(SettingsError::Io);
        }
        let mut next = self.stored.clone();
        next.wizard = w;
        let json = serde_json::to_vec_pretty(&next).map_err(|_| SettingsError::Io)?;
        write_atomic(&self.path, &json).map_err(|_| SettingsError::Io)?;
        self.stored = next;
        Ok(())
    }

    /// Current settings with the effective vision mode.
    pub fn settings(&self) -> Settings {
        Settings {
            local_only: self.stored.local_only,
            vision_mode: effective_vision(self.stored.vision_mode, &self.stored.profile),
            profile: self.stored.profile.clone(),
            provider: self.stored.provider,
            hotkey: self.stored.hotkey.clone(),
            kill_hotkey: self.stored.kill_hotkey.clone(),
            has_api_key: false,
            anthropic_model: self.stored.anthropic_model.clone(),
            models_dir: self.stored.models_dir.clone(),
            default_models_dir: String::new(),
            observe_mode: self.stored.observe_mode,
            agent_enabled: self.stored.agent_enabled,
            agent_mode: self.stored.agent_mode,
            diagnostic_logs: self.stored.diagnostic_logs,
        }
    }

    /// Validate `patch` field by field, persist atomically, then commit. An invalid value does
    /// not fail the update: it falls back to the safest value, the rejected text is dropped
    /// and an issue is returned. Only a failed write is an error (nothing changes then).
    pub fn apply_with_issues(
        &mut self,
        patch: &SettingsPatch,
    ) -> Result<(Settings, Vec<SettingsIssue>), SettingsError> {
        let mut next = self.stored.clone();
        if let Some(v) = patch.local_only {
            next.local_only = v;
        }
        if let Some(v) = patch.vision_mode {
            next.vision_mode = v;
        }
        if let Some(v) = &patch.profile {
            next.profile = v.clone();
        }
        if let Some(v) = patch.provider {
            next.provider = v;
        }
        if let Some(v) = &patch.hotkey {
            next.hotkey = v.clone();
        }
        if let Some(v) = &patch.kill_hotkey {
            next.kill_hotkey = v.clone();
        }
        if let Some(v) = &patch.anthropic_model {
            next.anthropic_model = v.clone();
        }
        if let Some(v) = &patch.models_dir {
            next.models_dir = v.clone();
        }
        if let Some(v) = patch.observe_mode {
            next.observe_mode = v;
        }
        if let Some(v) = patch.agent_enabled {
            next.agent_enabled = v;
        }
        if let Some(v) = patch.agent_mode {
            next.agent_mode = v;
        }
        if let Some(v) = patch.diagnostic_logs {
            next.diagnostic_logs = v;
        }
        let (next, issues) = next.sanitize(Some(patch));
        let json = serde_json::to_vec_pretty(&next).map_err(|_| SettingsError::Io)?;
        write_atomic(&self.path, &json).map_err(|_| SettingsError::Io)?;
        self.stored = next;
        self.load_error = None;
        self.load_issues.clear();
        Ok((self.settings(), issues))
    }

    /// [`apply_with_issues`](Self::apply_with_issues) without the issues.
    pub fn apply(&mut self, patch: &SettingsPatch) -> Result<Settings, SettingsError> {
        self.apply_with_issues(patch).map(|(s, _)| s)
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let d = tempfile::tempdir().unwrap();
        let s = SettingsStore::load(d.path().join("cfg").join("settings.json"));
        (d, s)
    }

    #[test]
    fn missing_file_gives_defaults_without_error() {
        let (_d, s) = store();
        assert_eq!(s.settings(), Settings::default());
        assert!(s.load_error().is_none());
    }

    #[test]
    fn update_persists_and_reloads() {
        let (d, mut s) = store();
        let out = s
            .apply(&SettingsPatch {
                local_only: Some(false),
                vision_mode: Some(VisionMode::Always),
                hotkey: Some("ctrl+shift+k".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(!out.local_only);
        assert_eq!(out.vision_mode, VisionMode::Always);
        assert_eq!(out.hotkey, "Ctrl+Shift+K");
        let again = SettingsStore::load(d.path().join("cfg").join("settings.json"));
        assert_eq!(again.settings(), out);
        // No temp file left behind and no secret-ish fields in the file.
        let dir: Vec<_> = fs::read_dir(d.path().join("cfg")).unwrap().collect();
        assert_eq!(dir.len(), 1);
        let text = fs::read_to_string(d.path().join("cfg").join("settings.json")).unwrap();
        assert!(!text.contains("api_key"));
    }

    #[test]
    fn low_memory_forces_vision_off_but_keeps_preference() {
        let (d, mut s) = store();
        s.apply(&SettingsPatch {
            vision_mode: Some(VisionMode::Always),
            ..Default::default()
        })
        .unwrap();
        let out = s
            .apply(&SettingsPatch {
                profile: Some("low-memory".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(out.vision_mode, VisionMode::Off);
        // Even an explicit request for Always yields Off on low-memory.
        let out = s
            .apply(&SettingsPatch {
                vision_mode: Some(VisionMode::Auto),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(out.vision_mode, VisionMode::Off);
        let again = SettingsStore::load(d.path().join("cfg").join("settings.json"));
        assert_eq!(again.settings().vision_mode, VisionMode::Off);
        // Back to default: the stored preference applies again.
        let out = s
            .apply(&SettingsPatch {
                profile: Some("default".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(out.vision_mode, VisionMode::Auto);
    }

    #[test]
    fn corrupt_file_gives_defaults_and_error_then_save_heals() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        for bad in [
            "{not json",
            "42",
            r#"{"profile":"nope"}"#,
            r#"{"hotkey":"A"}"#,
        ] {
            fs::write(&p, bad).unwrap();
            let mut s = SettingsStore::load(&p);
            assert_eq!(s.settings(), Settings::default(), "{bad}");
            assert_eq!(s.load_error(), Some(ERR_SETTINGS_CORRUPT), "{bad}");
            s.apply(&SettingsPatch::default()).unwrap();
            assert!(s.load_error().is_none());
            assert_eq!(SettingsStore::load(&p).load_error(), None);
        }
    }

    #[test]
    fn partial_file_fills_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, r#"{"local_only":false}"#).unwrap();
        let s = SettingsStore::load(&p);
        assert!(s.load_error().is_none());
        assert!(!s.settings().local_only);
        assert_eq!(s.settings().hotkey, DEFAULT_HOTKEY);
    }

    #[test]
    fn save_failure_is_reported_and_state_unchanged() {
        let d = tempfile::tempdir().unwrap();
        // Parent "directory" is a file: create_dir_all fails.
        let blocker = d.path().join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let mut s = SettingsStore::load(blocker.join("settings.json"));
        let err = s
            .apply(&SettingsPatch {
                local_only: Some(false),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, SettingsError::Io);
        assert!(s.settings().local_only);
    }

    #[test]
    fn provider_defaults_to_local_ollama_persists_and_rejects_unknown_values() {
        let (d, mut s) = store();
        assert_eq!(s.settings().provider, ProviderChoice::Ollama);
        let out = s
            .apply(&SettingsPatch {
                provider: Some(ProviderChoice::Anthropic),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(out.provider, ProviderChoice::Anthropic);
        let path = d.path().join("cfg").join("settings.json");
        assert_eq!(
            SettingsStore::load(&path).settings().provider,
            ProviderChoice::Anthropic
        );
        // An unknown provider in the file is a corrupt file: defaults (local) and an error.
        fs::write(&path, br#"{"provider":"openai"}"#).unwrap();
        let bad = SettingsStore::load(&path);
        assert_eq!(bad.settings().provider, ProviderChoice::Ollama);
        assert_eq!(bad.load_error(), Some(ERR_SETTINGS_CORRUPT));
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"provider":"openai"}"#).is_err());
    }

    #[test]
    fn hotkey_normalisation() {
        assert_eq!(
            normalize_hotkey("Ctrl+Alt+Space").unwrap(),
            "Ctrl+Alt+Space"
        );
        assert_eq!(
            normalize_hotkey("alt + ctrl + space").unwrap(),
            "Ctrl+Alt+Space"
        );
        assert_eq!(
            normalize_hotkey("cmd+shift+f12").unwrap(),
            "Shift+Super+F12"
        );
        assert_eq!(normalize_hotkey("Control+7").unwrap(), "Ctrl+7");
        for bad in [
            "",
            "Space",
            "Ctrl",
            "Ctrl+",
            "Ctrl+Alt",
            "Shift+A",
            "Shift+Space",
            "Ctrl+AB",
            "Ctrl+A+B",
            "Ctrl+Ctrl+A",
            "Ctrl+F0",
            "Ctrl+F25",
            "F1",
            "Ctrl+;",
            "Ctrl+Alt+Space+",
        ] {
            assert_eq!(
                normalize_hotkey(bad),
                Err(SettingsError::InvalidHotkey),
                "{bad:?}"
            );
        }
        assert!(normalize_hotkey(&format!("Ctrl+{}", "a".repeat(100))).is_err());
    }

    #[test]
    fn profile_names_come_from_registry() {
        assert_eq!(profile_names(), vec!["default", "low-memory"]);
        assert_eq!(
            effective_vision(VisionMode::Always, "low-memory"),
            VisionMode::Off
        );
        assert_eq!(
            effective_vision(VisionMode::Always, "default"),
            VisionMode::Always
        );
        assert_eq!(
            effective_vision(VisionMode::Always, "unknown"),
            VisionMode::Off
        );
    }

    // ------------------------------------------------------------- S2-7 validation tables

    fn hostile_strings() -> Vec<String> {
        vec![
            "a\u{0}b".into(),
            "a\nb".into(),
            "a\u{7}b".into(),
            "\u{202E}evil".into(),
            "a\u{2066}b".into(),
            "a\u{200B}b".into(),
            "x".repeat(10_240),
            "../../etc/passwd".into(),
            "..\\..\\windows".into(),
            "\\\\host\\share".into(),
            "CON".into(),
            "C:evil".into(),
            "C:\\".into(),
            "/".into(),
            "link -> /etc".into(),
            "<script>".into(),
            "'; DROP TABLE x;--".into(),
        ]
    }

    fn patch_of(f: impl FnOnce(&mut SettingsPatch)) -> SettingsPatch {
        let mut p = SettingsPatch::default();
        f(&mut p);
        p
    }

    fn issue_codes(issues: &[SettingsIssue], field: SettingsField) -> Vec<IssueCode> {
        issues
            .iter()
            .filter(|i| i.field == field)
            .map(|i| i.code)
            .collect()
    }

    #[test]
    fn profile_table_valid_and_invalid() {
        let (_d, mut s) = store();
        for ok in ["default", "low-memory"] {
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.profile = Some(ok.into())))
                .unwrap();
            assert_eq!(out.profile, ok);
            assert!(issues.is_empty());
        }
        let mut bad: Vec<String> = hostile_strings();
        bad.extend(["", "Default", "default ", "nope", "low_memory"].map(String::from));
        for b in bad {
            s.apply(&patch_of(|p| p.profile = Some("low-memory".into())))
                .unwrap();
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.profile = Some(b.clone())))
                .unwrap();
            assert_eq!(out.profile, DEFAULT_PROFILE, "{b:.20?}");
            assert_eq!(
                issues,
                vec![SettingsIssue {
                    field: SettingsField::Profile,
                    code: IssueCode::Unknown
                }]
            );
        }
    }

    #[test]
    fn kill_hotkey_setting_validation_and_conflicts() {
        let (d, mut s) = store();
        let path = d.path().join("cfg").join("settings.json");
        assert_eq!(s.settings().kill_hotkey, DEFAULT_KILL_HOTKEY);
        // Valid.
        let (out, issues) = s
            .apply_with_issues(&patch_of(|p| p.kill_hotkey = Some("alt+shift+f9".into())))
            .unwrap();
        assert_eq!(
            (out.kill_hotkey.as_str(), issues.len()),
            ("Alt+Shift+F9", 0)
        );
        assert_eq!(
            SettingsStore::load(&path).settings().kill_hotkey,
            "Alt+Shift+F9"
        );
        // One modifier, no modifier, the push-to-talk chord, hostile text: default + typed issue.
        for (bad, code) in [
            ("Ctrl+K", IssueCode::NeedsTwoModifiers),
            ("Shift+K", IssueCode::NeedsModifier),
            ("K", IssueCode::NeedsModifier),
            ("Ctrl+Alt+Space", IssueCode::Duplicate),
            ("ctrl+ALT+space", IssueCode::Duplicate),
            ("Ctrl+Alt+\u{202e}K", IssueCode::ControlChars),
            ("", IssueCode::InvalidFormat),
        ] {
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.kill_hotkey = Some(bad.into())))
                .unwrap();
            assert_eq!(out.kill_hotkey, DEFAULT_KILL_HOTKEY, "{bad:?}");
            assert_eq!(
                issue_codes(&issues, SettingsField::KillHotkey),
                vec![code],
                "{bad:?}"
            );
            let on_disk = std::fs::read_to_string(&path).unwrap();
            assert!(!on_disk.contains("202e") && !on_disk.contains("Ctrl+K"));
        }
        // Moving push-to-talk onto the kill chord is the rejected change.
        let (out, issues) = s
            .apply_with_issues(&patch_of(|p| p.hotkey = Some("ctrl+alt+shift+k".into())))
            .unwrap();
        assert_eq!(out.hotkey, DEFAULT_HOTKEY);
        assert_eq!(out.kill_hotkey, DEFAULT_KILL_HOTKEY);
        assert_eq!(
            issue_codes(&issues, SettingsField::Hotkey),
            vec![IssueCode::Duplicate]
        );
        // A stored file with both equal is not trusted.
        std::fs::write(
            &path,
            r#"{"hotkey":"Ctrl+Alt+K","kill_hotkey":"Ctrl+Alt+K"}"#,
        )
        .unwrap();
        let bad = SettingsStore::load(&path);
        assert_eq!(bad.load_error(), Some(ERR_SETTINGS_CORRUPT));
        assert_eq!(bad.settings().kill_hotkey, DEFAULT_KILL_HOTKEY);
    }

    #[test]
    fn hotkey_table_valid_boundary_and_hostile() {
        let (d, mut s) = store();
        let path = d.path().join("cfg").join("settings.json");
        let ok = [
            ("ctrl+alt+space", "Ctrl+Alt+Space"),
            ("Alt+K", "Alt+K"),
            ("super+1", "Super+1"),
            ("Ctrl+F24", "Ctrl+F24"),
            ("  Ctrl + Shift + a ", "Ctrl+Shift+A"),
        ];
        for (input, want) in ok {
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.hotkey = Some(input.into())))
                .unwrap();
            assert_eq!((out.hotkey.as_str(), issues.len()), (want, 0), "{input}");
        }
        let long = format!("Ctrl+{}", "a".repeat(10_240));
        let cases: Vec<(String, IssueCode)> = vec![
            ("Space".into(), IssueCode::NeedsModifier),
            ("Shift+A".into(), IssueCode::NeedsModifier),
            ("F1".into(), IssueCode::NeedsModifier),
            ("Ctrl".into(), IssueCode::InvalidFormat),
            ("".into(), IssueCode::InvalidFormat),
            ("Ctrl+Ctrl+A".into(), IssueCode::InvalidFormat),
            ("Ctrl+F25".into(), IssueCode::InvalidFormat),
            ("Ctrl+\u{202E}A".into(), IssueCode::ControlChars),
            ("Ctrl+A\u{0}".into(), IssueCode::ControlChars),
            ("Ctrl+A\n".into(), IssueCode::ControlChars),
            (long, IssueCode::TooLong),
            ("a".repeat(65), IssueCode::TooLong),
            ("Ctrl+../..".into(), IssueCode::InvalidFormat),
        ];
        for (input, code) in cases {
            s.apply(&patch_of(|p| p.hotkey = Some("Alt+K".into())))
                .unwrap();
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.hotkey = Some(input.clone())))
                .unwrap();
            // Safest value = the default hotkey; the rejected text is stored nowhere.
            assert_eq!(out.hotkey, DEFAULT_HOTKEY, "{input:.20?}");
            assert_eq!(issue_codes(&issues, SettingsField::Hotkey), vec![code]);
            let v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                v["hotkey"], DEFAULT_HOTKEY,
                "the rejected text is not stored"
            );
        }
        // The 64-byte boundary is accepted when the chord is otherwise valid.
        assert!(s
            .apply_with_issues(&patch_of(
                |p| p.hotkey = Some(format!("Ctrl+Alt+Shift+Super+{}{}", " ".repeat(30), "a"))
            ))
            .unwrap()
            .1
            .is_empty());
    }

    #[test]
    fn model_id_table_valid_boundary_and_hostile() {
        let (d, mut s) = store();
        let path = d.path().join("cfg").join("settings.json");
        for (input, want) in [
            (" some-model.v1:x_2 ", "some-model.v1:x_2"),
            ("", ""),
            (&"x".repeat(80), &"x".repeat(80)),
        ] {
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.anthropic_model = Some(input.into())))
                .unwrap();
            assert_eq!(out.anthropic_model, want);
            assert!(issues.is_empty());
        }
        let mut cases: Vec<(String, IssueCode)> = vec![
            ("a b".into(), IssueCode::InvalidFormat),
            ("a/b".into(), IssueCode::InvalidFormat),
            ("m\u{e9}".into(), IssueCode::InvalidFormat),
            ("x".repeat(81), IssueCode::TooLong),
            ("x".repeat(10_240), IssueCode::TooLong),
            ("a\nb".into(), IssueCode::ControlChars),
            ("a\u{0}".into(), IssueCode::ControlChars),
        ];
        cases.push(("../x".into(), IssueCode::InvalidFormat));
        cases.push(("\u{202E}m".into(), IssueCode::ControlChars));
        for (input, code) in cases {
            s.apply(&patch_of(|p| p.anthropic_model = Some("keep-me".into())))
                .unwrap();
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.anthropic_model = Some(input.clone())))
                .unwrap();
            // Safest value: no model id, so cloud stays unavailable.
            assert_eq!(out.anthropic_model, "", "{input:.20?}");
            assert!(!out.cloud_configured());
            assert_eq!(
                issue_codes(&issues, SettingsField::AnthropicModel),
                vec![code]
            );
            assert!(!fs::read_to_string(&path).unwrap().contains(input.trim()));
        }
        assert_eq!(validate_model_id("a b"), Err(SettingsError::InvalidModel));
    }

    #[test]
    fn models_dir_table_valid_and_hostile_with_fallback_to_default() {
        let (d, mut s) = store();
        let path = d.path().join("cfg").join("settings.json");
        let good = d.path().join("m").to_str().unwrap().to_string();
        let (out, issues) = s
            .apply_with_issues(&patch_of(|p| p.models_dir = Some(format!(" {good}/ "))))
            .unwrap();
        assert_eq!(out.models_dir, good);
        assert!(issues.is_empty());
        assert_eq!(SettingsStore::load(&path).settings().models_dir, good);
        // Empty goes back to the default.
        let (out, issues) = s
            .apply_with_issues(&patch_of(|p| p.models_dir = Some(String::new())))
            .unwrap();
        assert_eq!(out.models_dir, "");
        assert!(issues.is_empty());

        let mut bad = hostile_strings();
        bad.extend(
            [
                "relative/dir",
                "C:folder",
                "/tmp/../etc",
                "/etc/hey",
                "\\\\server\\share\\models",
                "C:\\CON",
                "/dev/null/x",
            ]
            .map(String::from),
        );
        for b in bad {
            s.apply(&patch_of(|p| p.models_dir = Some(good.clone())))
                .unwrap();
            let (out, issues) = s
                .apply_with_issues(&patch_of(|p| p.models_dir = Some(b.clone())))
                .unwrap();
            assert_eq!(out.models_dir, "", "{b:.20?}");
            assert_eq!(
                issues.iter().map(|i| i.field).collect::<Vec<_>>(),
                vec![SettingsField::ModelsDir],
                "{b:.20?}"
            );
            let file = fs::read_to_string(&path).unwrap();
            assert!(!file.contains(b.trim()) || b.trim().is_empty(), "{b:.20?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn models_dir_symlink_and_file_are_rejected() {
        let (d, mut s) = store();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(d.path(), &link).unwrap();
        let file = d.path().join("file");
        fs::write(&file, b"x").unwrap();
        for (p, code) in [
            (&link, IssueCode::Symlink),
            (&file, IssueCode::NotADirectory),
        ] {
            let (out, issues) = s
                .apply_with_issues(&patch_of(|x| {
                    x.models_dir = Some(p.to_str().unwrap().into())
                }))
                .unwrap();
            assert_eq!(out.models_dir, "");
            assert_eq!(issue_codes(&issues, SettingsField::ModelsDir), vec![code]);
        }
    }

    #[test]
    fn one_bad_field_does_not_block_the_good_ones() {
        let (_d, mut s) = store();
        let (out, issues) = s
            .apply_with_issues(&SettingsPatch {
                local_only: Some(false),
                profile: Some("low-memory".into()),
                hotkey: Some("Ctrl+Ctrl+A".into()),
                anthropic_model: Some("good-model".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(!out.local_only);
        assert_eq!(out.profile, "low-memory");
        assert_eq!(out.anthropic_model, "good-model");
        assert_eq!(out.hotkey, DEFAULT_HOTKEY);
        assert_eq!(
            issues,
            vec![SettingsIssue {
                field: SettingsField::Hotkey,
                code: IssueCode::InvalidFormat
            }]
        );
    }

    #[test]
    fn unrelated_save_does_not_reset_a_folder_that_vanished() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let dir = d.path().join("models");
        fs::create_dir(&dir).unwrap();
        let mut s = SettingsStore::load(&p);
        s.apply(&patch_of(|x| {
            x.models_dir = Some(dir.to_str().unwrap().into())
        }))
        .unwrap();
        fs::remove_dir(&dir).unwrap();
        fs::write(&dir, b"now a file").unwrap();
        // Unrelated change: the stored folder is left alone (the patch did not name it).
        let out = s.apply(&patch_of(|x| x.local_only = Some(false))).unwrap();
        assert_eq!(out.models_dir, dir.to_str().unwrap());
        // On the next start it is validated, replaced by the default and reported.
        let again = SettingsStore::load(&p);
        assert_eq!(again.settings().models_dir, "");
        assert_eq!(
            again.load_issues(),
            [SettingsIssue {
                field: SettingsField::ModelsDir,
                code: IssueCode::NotADirectory
            }]
        );
        assert!(again.load_error().is_none());
        assert!(!again.settings().local_only, "other settings are kept");
    }

    #[test]
    fn corrupt_values_in_the_file_fall_back_to_all_defaults_with_an_error() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        for bad in [
            r#"{"local_only":false,"hotkey":"Space"}"#,
            r#"{"local_only":false,"anthropic_model":"a b"}"#,
            r#"{"local_only":false,"profile":"nope"}"#,
            "\u{0}\u{0}",
            "",
        ] {
            fs::write(&p, bad).unwrap();
            let s = SettingsStore::load(&p);
            assert_eq!(s.settings(), Settings::default(), "{bad}");
            assert!(s.settings().local_only, "safest value");
            assert_eq!(s.load_error(), Some(ERR_SETTINGS_CORRUPT));
        }
    }

    #[test]
    fn written_file_is_atomic_and_has_no_secrets() {
        let (d, mut s) = store();
        s.apply(&patch_of(|x| x.anthropic_model = Some("m".into())))
            .unwrap();
        let cfg = d.path().join("cfg");
        let names: Vec<_> = fs::read_dir(&cfg)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["settings.json".to_string()]);
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(cfg.join("settings.json")).unwrap()).unwrap();
        for key in v.as_object().unwrap().keys() {
            let k = key.to_lowercase();
            assert!(
                !k.contains("api_key") && !k.contains("secret") && !k.contains("token"),
                "{key}"
            );
        }
        // A stale temp file from a crash does not break the next save.
        fs::write(cfg.join("settings.json.tmp"), b"half").unwrap();
        s.apply(&patch_of(|x| x.local_only = Some(false))).unwrap();
        assert!(
            !SettingsStore::load(cfg.join("settings.json"))
                .settings()
                .local_only
        );
        assert!(!cfg.join("settings.json.tmp").exists());
    }

    // ------------------------------------------------------------- safe-default rules

    #[test]
    fn safe_defaults() {
        let d = Settings::default();
        assert!(d.local_only, "local_only defaults to on");
        assert_eq!(d.vision_mode, VisionMode::Auto);
        assert_eq!(d.provider, ProviderChoice::Ollama);
        assert_eq!(d.profile, DEFAULT_PROFILE);
        assert_eq!(d.hotkey, DEFAULT_HOTKEY);
        assert!(d.anthropic_model.is_empty() && !d.cloud_configured());
        assert!(d.models_dir.is_empty());
        // The default hotkey passes its own validation.
        assert_eq!(normalize_hotkey(DEFAULT_HOTKEY).unwrap(), DEFAULT_HOTKEY);
        // Vision is forced off wherever the profile has no VLM, whatever the stored choice.
        for m in [VisionMode::Off, VisionMode::Auto, VisionMode::Always] {
            assert_eq!(effective_vision(m, "low-memory"), VisionMode::Off);
        }
    }

    #[test]
    fn no_profile_runs_stt_on_the_gpu() {
        for name in profile_names() {
            let p = builtin_registry().profile(&name).unwrap();
            assert_ne!(p.stt.device, voice::registry::Device::Gpu, "{name}");
            assert!(crate::buddy::engine_models(builtin_registry(), &name).is_ok());
        }
    }

    #[test]
    fn agent_settings_default_safe_persist_and_agent_mode_needs_agent_enabled() {
        let (d, mut s) = store();
        let def = s.settings();
        assert!(def.observe_mode, "observe mode (dry run first) defaults ON");
        assert!(
            !def.agent_enabled && !def.agent_mode,
            "the agent defaults OFF"
        );
        // agent_mode alone does nothing.
        let out = s.apply(&patch_of(|x| x.agent_mode = Some(true))).unwrap();
        assert!(!out.agent_mode);
        let out = s
            .apply(&patch_of(|x| {
                x.agent_enabled = Some(true);
                x.agent_mode = Some(true);
                x.observe_mode = Some(false);
            }))
            .unwrap();
        assert!(out.agent_enabled && out.agent_mode && !out.observe_mode);
        // Persisted.
        let again = SettingsStore::load(d.path().join("cfg").join("settings.json"));
        assert_eq!(again.settings(), out);
        // Turning the agent off turns the mode off with it.
        let out = s
            .apply(&patch_of(|x| x.agent_enabled = Some(false)))
            .unwrap();
        assert!(!out.agent_mode && !out.observe_mode);
        // An unreadable file gives the safe defaults.
        fs::write(d.path().join("cfg").join("settings.json"), b"{bad").unwrap();
        let bad = SettingsStore::load(d.path().join("cfg").join("settings.json"));
        assert!(bad.settings().observe_mode && !bad.settings().agent_enabled);
    }

    /// Every field of every settings type, by name. Adding a field makes this test fail until
    /// the new name is added HERE, which is the moment to confirm it is not a switch for the
    /// firewall, the OCR / redaction stages, the deny-list defaults, the preview gate or
    /// push-to-talk. No setting may ever do that (principles 1 and 2).
    const ALLOWED_FIELDS: &[&str] = &[
        // Settings
        "local_only",  // forbids cloud calls; turning it off still needs preview + Send
        "vision_mode", // Off/Auto/Always for the screenshot, never the redaction
        "profile",     // which models load
        "provider",    // ollama or anthropic
        "hotkey",      // the push-to-talk chord (must include Ctrl/Alt/Super)
        "kill_hotkey", // the kill-switch chord (can only STOP things; two modifiers, never the PTT chord)
        "has_api_key", // read-only display flag, never the key
        "anthropic_model", // cloud model id
        "models_dir",  // where model files live
        "default_models_dir", // display only
        "observe_mode", // dry run first (default ON); turning it off only skips that question, never a check
        "agent_enabled", // opt-in to the agent (default OFF); the readiness checks still apply
        "agent_mode",   // route voice requests to the agent (default OFF)
        "diagnostic_logs", // local, size-capped, content-free log file (default OFF)
    ];

    /// Words that would name a safety switch. Checked on top of the allowlist.
    const FORBIDDEN_WORDS: &[&str] = &[
        "firewall",
        "redact",
        "ocr",
        "blackout",
        "deny",
        "preview",
        "consent",
        "confirm",
        "gate",
        "ptt",
        "push_to_talk",
        "always_on",
        "always_listen",
        "mic",
        "disable",
        "bypass",
        "skip",
        "unsafe",
        "debug",
        "gpu",
    ];

    fn field_names(v: serde_json::Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn no_setting_switches_off_a_safety_stage() {
        let mut want: Vec<String> = ALLOWED_FIELDS.iter().map(|s| s.to_string()).collect();
        want.sort();
        // What the UI sees, what is stored, and what a patch can set.
        let shown = field_names(serde_json::to_value(Settings::default()).unwrap());
        assert_eq!(
            shown, want,
            "Settings fields changed: update ALLOWED_FIELDS consciously"
        );
        // `wizard` is first-run progress (S3-4): stored, but never settable through a patch
        // (only `SettingsStore::set_wizard`, which takes a validated state).
        let mut stored: Vec<String> = field_names(serde_json::to_value(Stored::default()).unwrap());
        assert!(stored.contains(&"wizard".to_string()));
        stored.retain(|f| f != "wizard");
        for f in &stored {
            assert!(want.contains(f), "stored field {f} is not allow-listed");
        }
        let patch = field_names(
            serde_json::to_value(SettingsPatch {
                local_only: Some(true),
                vision_mode: Some(VisionMode::Off),
                profile: Some(String::new()),
                provider: Some(ProviderChoice::Ollama),
                hotkey: Some(String::new()),
                kill_hotkey: Some(String::new()),
                anthropic_model: Some(String::new()),
                models_dir: Some(String::new()),
                observe_mode: Some(true),
                agent_enabled: Some(false),
                agent_mode: Some(false),
                diagnostic_logs: Some(false),
            })
            .unwrap(),
        );
        for f in &patch {
            assert!(want.contains(f), "patch field {f} is not allow-listed");
        }
        assert_eq!(patch, stored, "a patch sets exactly the stored fields");
        for f in shown.iter().chain(&stored).chain(&patch) {
            for w in FORBIDDEN_WORDS {
                assert!(!f.contains(w), "field {f} looks like a safety switch ({w})");
            }
        }
        // An unknown key in a patch is ignored by serde, so it can never reach a setting.
        let p: SettingsPatch =
            serde_json::from_str(r#"{"disable_firewall":true,"skip_preview":true}"#).unwrap();
        assert_eq!(p, SettingsPatch::default());
    }
}
