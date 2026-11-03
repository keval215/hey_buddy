//! Settings validation and persistence. Pure logic plus one JSON file in the app config dir.
//! Written atomically (temp file + rename). Never stores secrets (the API key lives in the OS
//! keychain, T11). Errors carry fixed text only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use voice::registry::Registry;

use crate::api::{Settings, SettingsPatch, VisionMode, DEFAULT_HOTKEY, DEFAULT_PROFILE};
use crate::route::effective_vision_mode;

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
    let s = s.trim();
    if s.len() > MAX_MODEL_ID_LEN
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(SettingsError::InvalidModel);
    }
    Ok(s.to_string())
}

/// Validate and canonicalise a push-to-talk shortcut: at least one of Ctrl, Alt or Super (Cmd)
/// (optionally with Shift) plus exactly one key (A-Z, 0-9, F1-F24, Space), e.g.
/// `ctrl+alt+space` -> `Ctrl+Alt+Space`. A bare key or a Shift-only chord is rejected so the
/// hotkey never steals typing.
pub fn normalize_hotkey(s: &str) -> Result<String, SettingsError> {
    if s.len() > 64 {
        return Err(SettingsError::InvalidHotkey);
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
                    return Err(SettingsError::InvalidHotkey);
                }
                *flag = true;
            }
            None => {
                if key.is_some() {
                    return Err(SettingsError::InvalidHotkey);
                }
                key = Some(normalize_key(p).ok_or(SettingsError::InvalidHotkey)?);
            }
        }
    }
    let key = key.ok_or(SettingsError::InvalidHotkey)?;
    if !(ctrl || alt || sup) {
        return Err(SettingsError::InvalidHotkey);
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
    hotkey: String,
    anthropic_model: String,
}

impl Default for Stored {
    fn default() -> Self {
        let d = Settings::default();
        Self {
            local_only: d.local_only,
            vision_mode: d.vision_mode,
            profile: DEFAULT_PROFILE.into(),
            hotkey: DEFAULT_HOTKEY.into(),
            anthropic_model: String::new(),
        }
    }
}

impl Stored {
    fn validate(&self) -> Result<Stored, SettingsError> {
        if registry().profile(&self.profile).is_none() {
            return Err(SettingsError::InvalidProfile);
        }
        Ok(Stored {
            hotkey: normalize_hotkey(&self.hotkey)?,
            anthropic_model: validate_model_id(&self.anthropic_model)?,
            ..self.clone()
        })
    }
}

#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    stored: Stored,
    load_error: Option<&'static str>,
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
        };
        match fs::read(&store.path) {
            Ok(bytes) => {
                let parsed = serde_json::from_slice::<Stored>(&bytes)
                    .map_err(|_| ())
                    .and_then(|s| s.validate().map_err(|_| ()));
                match parsed {
                    Ok(s) => store.stored = s,
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

    /// Current settings with the effective vision mode.
    pub fn settings(&self) -> Settings {
        Settings {
            local_only: self.stored.local_only,
            vision_mode: effective_vision(self.stored.vision_mode, &self.stored.profile),
            profile: self.stored.profile.clone(),
            hotkey: self.stored.hotkey.clone(),
            has_api_key: false,
            anthropic_model: self.stored.anthropic_model.clone(),
        }
    }

    /// Validate `patch`, persist atomically, then commit. On any error nothing changes.
    pub fn apply(&mut self, patch: &SettingsPatch) -> Result<Settings, SettingsError> {
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
        if let Some(v) = &patch.hotkey {
            next.hotkey = v.clone();
        }
        if let Some(v) = &patch.anthropic_model {
            next.anthropic_model = v.clone();
        }
        let next = next.validate()?;
        let json = serde_json::to_vec_pretty(&next).map_err(|_| SettingsError::Io)?;
        write_atomic(&self.path, &json).map_err(|_| SettingsError::Io)?;
        self.stored = next;
        self.load_error = None;
        Ok(self.settings())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    fn invalid_patches_change_nothing() {
        let (_d, mut s) = store();
        let before = s.settings();
        for p in [
            SettingsPatch {
                profile: Some("nope".into()),
                ..Default::default()
            },
            SettingsPatch {
                hotkey: Some("Space".into()),
                ..Default::default()
            },
            SettingsPatch {
                // valid field plus an invalid one: all-or-nothing
                local_only: Some(false),
                hotkey: Some("Ctrl+Ctrl+A".into()),
                ..Default::default()
            },
        ] {
            assert!(s.apply(&p).is_err());
            assert_eq!(s.settings(), before);
        }
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
    fn model_id_validation() {
        let (_d, mut s) = store();
        let ok = |m: &str| SettingsPatch {
            anthropic_model: Some(m.into()),
            ..Default::default()
        };
        assert_eq!(
            s.apply(&ok(" some-model.v1:x_2 ")).unwrap().anthropic_model,
            "some-model.v1:x_2"
        );
        assert!(s.apply(&ok("")).unwrap().anthropic_model.is_empty());
        for bad in ["a b", "a/b", "m\u{e9}", &"x".repeat(81), "a\nb"] {
            assert_eq!(
                s.apply(&ok(bad)),
                Err(SettingsError::InvalidModel),
                "{bad:?}"
            );
        }
        assert!(s.apply(&ok(&"x".repeat(80))).is_ok());
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
}
