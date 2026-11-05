//! Model registry loader. Models carry no licence gate (D-004): `commercial_ok` is
//! information only. Weights are never bundled, so `bundled` must be false (fail closed).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot read registry: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid registry JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("model `{0}`: bundled must be false")]
    Bundled(String),
    #[error("model `{id}`: invalid sha256 `{value}` (expected TODO or 64 lowercase hex)")]
    BadSha256 { id: String, value: String },
    #[error("model `{id}`: unsafe file path `{path}`")]
    UnsafePath { id: String, path: String },
    #[error(
        "model `{0}`: `files` must be non-empty, unique, and url/sha256 must be TODO next to it"
    )]
    BadFiles(String),
    #[error("duplicate model id `{0}`")]
    DuplicateId(String),
    #[error("more than one default model for task {0:?}")]
    DuplicateDefault(Task),
    #[error("unsupported schema_version {0}")]
    UnsupportedVersion(u32),
    #[error("model `{0}` not found")]
    NotFound(String),
    #[error("profile `{profile}`: model `{model}` does not exist")]
    ProfileUnknownModel { profile: String, model: String },
    #[error("profile `{profile}`: model `{model}` is not a {task:?} model")]
    ProfileTaskMismatch {
        profile: String,
        model: String,
        task: Task,
    },
    #[error("profile `{0}`: STT must never run on the GPU")]
    SttOnGpu(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    Stt,
    Tts,
    Vad,
    /// Text LLM (served by Ollama).
    Llm,
    /// Vision-language model (served by Ollama).
    Vlm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Cpu,
    Gpu,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id: String,
    pub task: Task,
    pub size: String,
    pub licence: String,
    /// Information only; never gates loading (D-004).
    pub commercial_ok: bool,
    pub bundled: bool,
    pub url: String,
    pub sha256: String,
    pub languages: Vec<String>,
    pub default: bool,
    /// Ollama model tag (llm/vlm entries), `TODO-verify` where unsure.
    #[serde(default)]
    pub ollama_tag: Option<String>,
    /// Exact download size in bytes, when known (sanity limit + disk-space display).
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// Directory-shaped or multi-file models (S2-4): every file with its own url and sha256.
    /// `path` is relative to the models directory (e.g. `phonon-31m/model.safetensors`).
    /// When present, the entry-level `url` and `sha256` must stay `TODO`.
    #[serde(default)]
    pub files: Option<Vec<ModelFile>>,
}

/// One downloadable file of a multi-file model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    /// Relative to the models directory; `/` separators only (see [`is_safe_rel_path`]).
    pub path: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A registry file path is safe when it is relative, uses `/` only and every component is a
/// plain name of `[A-Za-z0-9._-]` (this rules out absolute paths, `..`, backslashes, drive
/// letters, `:` alternate streams, UNC prefixes, trailing dots and Windows device names).
pub fn is_safe_rel_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path.split('/').all(|c| {
            !c.is_empty()
                // No dot-prefixed names: keeps `.downloads` (our bookkeeping) and `.git`-likes out.
                && !c.starts_with('.')
                && !c.ends_with('.')
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                && !WINDOWS_RESERVED.contains(
                    &c.split('.')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .as_str(),
                )
        })
}

/// One task assignment inside a profile.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub model: String,
    pub device: Device,
}

/// A hardware profile: task -> model id + device hint. `vlm: null` means disabled.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub description: String,
    pub stt: Assignment,
    pub tts: Assignment,
    pub llm: Assignment,
    pub vlm: Option<Assignment>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub models: Vec<ModelEntry>,
    pub profiles: BTreeMap<String, Profile>,
}

fn sha_ok(s: &str) -> bool {
    s == "TODO" || (s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

impl Registry {
    pub fn from_json_str(json: &str) -> Result<Self, RegistryError> {
        let reg: Registry = serde_json::from_str(json)?;
        reg.validate()?;
        Ok(reg)
    }

    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        Self::from_json_str(&std::fs::read_to_string(path)?)
    }

    fn validate(&self) -> Result<(), RegistryError> {
        if self.schema_version != 1 {
            return Err(RegistryError::UnsupportedVersion(self.schema_version));
        }
        for (i, m) in self.models.iter().enumerate() {
            if self.models[..i].iter().any(|p| p.id == m.id) {
                return Err(RegistryError::DuplicateId(m.id.clone()));
            }
            if m.default
                && self.models[..i]
                    .iter()
                    .any(|p| p.task == m.task && p.default)
            {
                return Err(RegistryError::DuplicateDefault(m.task));
            }
            if m.bundled {
                return Err(RegistryError::Bundled(m.id.clone()));
            }
            if !sha_ok(&m.sha256) {
                return Err(RegistryError::BadSha256 {
                    id: m.id.clone(),
                    value: m.sha256.clone(),
                });
            }
            if let Some(files) = &m.files {
                if files.is_empty() || m.url != "TODO" || m.sha256 != "TODO" {
                    return Err(RegistryError::BadFiles(m.id.clone()));
                }
                for (j, f) in files.iter().enumerate() {
                    if !is_safe_rel_path(&f.path) {
                        return Err(RegistryError::UnsafePath {
                            id: m.id.clone(),
                            path: f.path.clone(),
                        });
                    }
                    if files[..j]
                        .iter()
                        .any(|p| p.path.eq_ignore_ascii_case(&f.path))
                    {
                        return Err(RegistryError::BadFiles(m.id.clone()));
                    }
                    if !sha_ok(&f.sha256) {
                        return Err(RegistryError::BadSha256 {
                            id: m.id.clone(),
                            value: f.sha256.clone(),
                        });
                    }
                }
            }
        }
        for (name, p) in &self.profiles {
            let check = |a: &Assignment, task: Task| {
                let m = self
                    .models
                    .iter()
                    .find(|m| m.id == a.model)
                    .ok_or_else(|| RegistryError::ProfileUnknownModel {
                        profile: name.clone(),
                        model: a.model.clone(),
                    })?;
                if m.task != task {
                    return Err(RegistryError::ProfileTaskMismatch {
                        profile: name.clone(),
                        model: a.model.clone(),
                        task,
                    });
                }
                Ok(())
            };
            check(&p.stt, Task::Stt)?;
            check(&p.tts, Task::Tts)?;
            check(&p.llm, Task::Llm)?;
            if let Some(v) = &p.vlm {
                check(v, Task::Vlm)?;
            }
            if p.stt.device == Device::Gpu {
                return Err(RegistryError::SttOnGpu(name.clone()));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&ModelEntry, RegistryError> {
        self.models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))
    }

    /// Resolve the on-disk path `<models_dir>/<id>.<ext>` for a registry model.
    /// Requires the file to exist (`VoiceError::ModelMissing` otherwise).
    /// Never downloads anything.
    pub fn model_path(
        &self,
        id: &str,
        models_dir: &Path,
        ext: &str,
    ) -> Result<std::path::PathBuf, crate::VoiceError> {
        let m = self.get(id)?;
        let path = models_dir.join(format!("{}.{ext}", m.id));
        if path.is_file() {
            Ok(path)
        } else {
            Err(crate::VoiceError::ModelMissing { path })
        }
    }

    /// Like [`Self::model_path`] for models shipped as a directory (e.g. Phonon checkpoints):
    /// `<models_dir>/<id>/`. `ModelMissing` if it is not a directory.
    pub fn model_dir(
        &self,
        id: &str,
        models_dir: &Path,
    ) -> Result<std::path::PathBuf, crate::VoiceError> {
        let m = self.get(id)?;
        let path = models_dir.join(&m.id);
        if path.is_dir() {
            Ok(path)
        } else {
            Err(crate::VoiceError::ModelMissing { path })
        }
    }

    /// The default model for a task.
    pub fn default_for(&self, task: Task) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.task == task && m.default)
    }

    /// A hardware profile by name (`default`, `low-memory`, ...).
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_entry() -> serde_json::Value {
        serde_json::json!({
            "id":"m","task":"stt","size":"1","licence":"MIT","commercial_ok":true,
            "bundled":false,"url":"TODO","sha256":"TODO","languages":["en"],"default":true
        })
    }

    fn entry(extra: &str) -> String {
        let mut base = base_entry();
        let over: serde_json::Value = serde_json::from_str(extra).unwrap();
        for (k, v) in over.as_object().unwrap() {
            if v.is_null() {
                base.as_object_mut().unwrap().remove(k);
            } else {
                base[k] = v.clone();
            }
        }
        format!(r#"{{"schema_version":1,"models":[{base}],"profiles":{{}}}}"#)
    }

    fn with_models(models: serde_json::Value, profiles: serde_json::Value) -> String {
        serde_json::json!({"schema_version":1,"models":models,"profiles":profiles}).to_string()
    }

    fn m(id: &str, task: &str, default: bool) -> serde_json::Value {
        let mut e = base_entry();
        e["id"] = id.into();
        e["task"] = task.into();
        e["default"] = default.into();
        e
    }

    fn real() -> Registry {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/registry.json");
        Registry::load(&p).unwrap()
    }

    #[test]
    fn valid_minimal() {
        let r = Registry::from_json_str(&entry("{}")).unwrap();
        assert_eq!(r.default_for(Task::Stt).unwrap().id, "m");
        let hex = "a".repeat(64);
        Registry::from_json_str(&entry(&format!(r#"{{"sha256":"{hex}"}}"#))).unwrap();
    }

    #[test]
    fn real_registry_parses() {
        let r = real();
        assert!(r.models.iter().all(|m| !m.bundled));
        // Exactly one default per task.
        for t in [Task::Stt, Task::Tts, Task::Vad, Task::Llm, Task::Vlm] {
            let n = r.models.iter().filter(|m| m.task == t && m.default).count();
            assert_eq!(n, 1, "{t:?}");
        }
        assert_eq!(r.default_for(Task::Stt).unwrap().id, "whisper-small");
        assert_eq!(r.default_for(Task::Tts).unwrap().id, "phonon-31m");
        assert_eq!(r.default_for(Task::Llm).unwrap().id, "qwen3-4b-q4");
        assert_eq!(r.default_for(Task::Vlm).unwrap().id, "qwen3-vl-2b");
        // Non-commercial models load without any flag; commercial_ok is informational.
        for id in ["phonon-40m", "fish-s2-pro"] {
            let e = r.get(id).unwrap();
            assert!(!e.commercial_ok && !e.default);
        }
        for id in ["qwen3-4b-q4", "qwen3-1.7b", "qwen3-vl-2b"] {
            assert!(r.get(id).unwrap().ollama_tag.is_some(), "{id}");
        }
    }

    #[test]
    fn real_profiles() {
        let r = real();
        let d = r.profile("default").unwrap();
        assert_eq!(
            d.stt,
            Assignment {
                model: "whisper-small".into(),
                device: Device::Cpu
            }
        );
        assert_eq!(d.tts.model, "phonon-31m");
        assert_eq!(d.tts.device, Device::Cpu);
        assert_eq!(d.llm.model, "qwen3-4b-q4");
        assert_eq!(d.llm.device, Device::Gpu);
        assert_eq!(d.vlm.as_ref().unwrap().model, "qwen3-vl-2b");
        let l = r.profile("low-memory").unwrap();
        assert_eq!(l.stt.model, "whisper-base");
        assert_eq!(l.llm.model, "qwen3-1.7b");
        assert!(l.vlm.is_none());
        assert!(r.profile("nope").is_none());
        for p in r.profiles.values() {
            assert_ne!(p.stt.device, Device::Gpu);
        }
    }

    #[test]
    fn non_commercial_default_allowed() {
        let r = Registry::from_json_str(&entry(r#"{"commercial_ok":false}"#)).unwrap();
        let d = r.default_for(Task::Stt).unwrap();
        assert!(!d.commercial_ok);
        assert!(r.get("m").is_ok());
    }

    #[test]
    fn model_path_missing_and_present() {
        let r = Registry::from_json_str(&entry("{}")).unwrap();
        let dir = std::env::temp_dir().join(format!("hb-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = r.model_path("m", &dir, "bin").unwrap_err();
        assert!(
            matches!(e, crate::VoiceError::ModelMissing { ref path } if path == &dir.join("m.bin"))
        );
        std::fs::write(dir.join("m.bin"), b"x").unwrap();
        assert_eq!(r.model_path("m", &dir, "bin").unwrap(), dir.join("m.bin"));
        assert!(matches!(
            r.model_path("nope", &dir, "bin"),
            Err(crate::VoiceError::Backend(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_dir_missing_and_present() {
        let r = Registry::from_json_str(&entry("{}")).unwrap();
        let dir = std::env::temp_dir().join(format!("hb-regd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = r.model_dir("m", &dir).unwrap_err();
        assert!(
            matches!(e, crate::VoiceError::ModelMissing { ref path } if path == &dir.join("m"))
        );
        std::fs::create_dir_all(dir.join("m")).unwrap();
        assert_eq!(r.model_dir("m", &dir).unwrap(), dir.join("m"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_checksum_rejected() {
        for bad in ["abc", &"A".repeat(64), &"g".repeat(64), ""] {
            let e =
                Registry::from_json_str(&entry(&format!(r#"{{"sha256":"{bad}"}}"#))).unwrap_err();
            assert!(matches!(e, RegistryError::BadSha256 { .. }), "{bad}");
        }
    }

    #[test]
    fn missing_fields_rejected() {
        for f in [
            "id",
            "task",
            "size",
            "licence",
            "commercial_ok",
            "bundled",
            "url",
            "sha256",
            "languages",
            "default",
        ] {
            let e = Registry::from_json_str(&entry(&format!(r#"{{"{f}":null}}"#))).unwrap_err();
            assert!(matches!(e, RegistryError::Parse(_)), "{f}");
        }
        // profiles section is required.
        let e = Registry::from_json_str(r#"{"schema_version":1,"models":[]}"#).unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)));
    }

    #[test]
    fn files_list_and_size_parse_and_validate() {
        let hex = "b".repeat(64);
        let ok = entry(&format!(
            r#"{{"size_bytes":10,"files":[{{"path":"m/a.bin","url":"TODO","sha256":"{hex}","size_bytes":5}},{{"path":"m/sub/b.json","url":"TODO","sha256":"TODO"}}]}}"#
        ));
        let r = Registry::from_json_str(&ok).unwrap();
        let e = r.get("m").unwrap();
        assert_eq!(e.size_bytes, Some(10));
        assert_eq!(e.files.as_ref().unwrap().len(), 2);
        // Unknown field inside a file entry is rejected.
        let bad = entry(r#"{"files":[{"path":"a","url":"TODO","sha256":"TODO","x":1}]}"#);
        assert!(matches!(
            Registry::from_json_str(&bad).unwrap_err(),
            RegistryError::Parse(_)
        ));
        // Empty list, entry-level url next to files, duplicate path, bad sha.
        for extra in [
            r#"{"files":[]}"#,
            r#"{"url":"https://x.example/a","files":[{"path":"a","url":"TODO","sha256":"TODO"}]}"#,
            r#"{"files":[{"path":"a","url":"TODO","sha256":"TODO"},{"path":"A","url":"TODO","sha256":"TODO"}]}"#,
        ] {
            assert!(matches!(
                Registry::from_json_str(&entry(extra)).unwrap_err(),
                RegistryError::BadFiles(_)
            ));
        }
        let e = Registry::from_json_str(&entry(
            r#"{"files":[{"path":"a","url":"TODO","sha256":"nothex"}]}"#,
        ))
        .unwrap_err();
        assert!(matches!(e, RegistryError::BadSha256 { .. }));
    }

    #[test]
    fn unsafe_file_paths_rejected() {
        for p in [
            "/etc/passwd",
            "../x",
            "a/../x",
            "a//b",
            "./a",
            "a\\b",
            "C:/x",
            "C:x",
            "a:stream",
            "\\\\srv\\share",
            "con",
            "NUL.txt",
            "a.",
            ".downloads",
            ".downloads/work/x",
            ".git/config",
            "a/.hidden",
            "",
            "a/",
            "caf\u{e9}",
        ] {
            assert!(!is_safe_rel_path(p), "{p}");
            let j = entry(&format!(
                r#"{{"files":[{{"path":{},"url":"TODO","sha256":"TODO"}}]}}"#,
                serde_json::to_string(p).unwrap()
            ));
            assert!(
                matches!(
                    Registry::from_json_str(&j).unwrap_err(),
                    RegistryError::UnsafePath { .. }
                ),
                "{p}"
            );
        }
        for p in [
            "a.bin",
            "kokoro-voices/af_heart.bin",
            "phonon-31m/w_1.safetensors",
        ] {
            assert!(is_safe_rel_path(p), "{p}");
        }
    }

    #[test]
    fn unknown_fields_rejected() {
        let e = Registry::from_json_str(&entry(r#"{"surprise":1}"#)).unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)));
    }

    #[test]
    fn bundled_true_rejected() {
        let e = Registry::from_json_str(&entry(r#"{"bundled":true}"#)).unwrap_err();
        assert!(matches!(e, RegistryError::Bundled(_)));
    }

    #[test]
    fn duplicate_id_rejected() {
        let j = with_models(
            serde_json::json!([m("a", "stt", true), m("a", "stt", false)]),
            serde_json::json!({}),
        );
        assert!(matches!(
            Registry::from_json_str(&j).unwrap_err(),
            RegistryError::DuplicateId(_)
        ));
    }

    #[test]
    fn duplicate_default_per_task_rejected() {
        for task in ["stt", "tts", "vad", "llm", "vlm"] {
            let j = with_models(
                serde_json::json!([m("a", task, true), m("b", task, true)]),
                serde_json::json!({}),
            );
            assert!(
                matches!(
                    Registry::from_json_str(&j).unwrap_err(),
                    RegistryError::DuplicateDefault(_)
                ),
                "{task}"
            );
        }
        // Defaults for different tasks are fine.
        let j = with_models(
            serde_json::json!([m("a", "stt", true), m("b", "tts", true)]),
            serde_json::json!({}),
        );
        Registry::from_json_str(&j).unwrap();
    }

    fn profile_json(stt_dev: &str, llm_id: &str, vlm: serde_json::Value) -> String {
        with_models(
            serde_json::json!([
                m("s", "stt", true),
                m("t", "tts", true),
                m("l", "llm", true),
                m("v", "vlm", true)
            ]),
            serde_json::json!({"p": {
                "description": "d",
                "stt": {"model":"s","device":stt_dev},
                "tts": {"model":"t","device":"cpu"},
                "llm": {"model":llm_id,"device":"gpu"},
                "vlm": vlm
            }}),
        )
    }

    #[test]
    fn profile_validation() {
        let ok = profile_json("cpu", "l", serde_json::json!({"model":"v","device":"gpu"}));
        assert!(Registry::from_json_str(&ok).unwrap().profile("p").is_some());
        let disabled = profile_json("cpu", "l", serde_json::Value::Null);
        assert!(Registry::from_json_str(&disabled)
            .unwrap()
            .profile("p")
            .unwrap()
            .vlm
            .is_none());

        let e = Registry::from_json_str(&profile_json("gpu", "l", serde_json::Value::Null))
            .unwrap_err();
        assert!(matches!(e, RegistryError::SttOnGpu(_)));
        let e = Registry::from_json_str(&profile_json("cpu", "ghost", serde_json::Value::Null))
            .unwrap_err();
        assert!(matches!(e, RegistryError::ProfileUnknownModel { .. }));
        // Existing model of the wrong task.
        let e = Registry::from_json_str(&profile_json("cpu", "s", serde_json::Value::Null))
            .unwrap_err();
        assert!(matches!(e, RegistryError::ProfileTaskMismatch { .. }));
        // Bad device value.
        let e = Registry::from_json_str(&profile_json("npu", "l", serde_json::Value::Null))
            .unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)));
    }
}
