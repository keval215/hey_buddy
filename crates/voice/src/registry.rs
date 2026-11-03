//! Model registry loader. Enforces licence rules (fail closed).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot read registry: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid registry JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("model `{0}`: non-commercial model cannot be a default")]
    NonCommercialDefault(String),
    #[error("model `{0}`: bundled must be false")]
    Bundled(String),
    #[error("model `{id}`: invalid sha256 `{value}` (expected TODO or 64 lowercase hex)")]
    BadSha256 { id: String, value: String },
    #[error("duplicate model id `{0}`")]
    DuplicateId(String),
    #[error("unsupported schema_version {0}")]
    UnsupportedVersion(u32),
    #[error("model `{0}` not found")]
    NotFound(String),
    #[error("model `{0}` is non-commercial; user must explicitly accept its licence")]
    LicenceNotAccepted(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    Stt,
    Tts,
    Vad,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id: String,
    pub task: Task,
    pub size: String,
    pub licence: String,
    pub commercial_ok: bool,
    pub bundled: bool,
    pub url: String,
    pub sha256: String,
    pub languages: Vec<String>,
    pub default: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub models: Vec<ModelEntry>,
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
            if m.bundled {
                return Err(RegistryError::Bundled(m.id.clone()));
            }
            if !m.commercial_ok && m.default {
                return Err(RegistryError::NonCommercialDefault(m.id.clone()));
            }
            if !sha_ok(&m.sha256) {
                return Err(RegistryError::BadSha256 {
                    id: m.id.clone(),
                    value: m.sha256.clone(),
                });
            }
        }
        Ok(())
    }

    /// Look up a model for use. Non-commercial models need `user_accepted_licence`.
    pub fn get(&self, id: &str, user_accepted_licence: bool) -> Result<&ModelEntry, RegistryError> {
        let m = self
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        if !m.commercial_ok && !user_accepted_licence {
            return Err(RegistryError::LicenceNotAccepted(m.id.clone()));
        }
        Ok(m)
    }

    /// Resolve the on-disk path `<models_dir>/<id>.<ext>` for a registry model.
    /// Applies the licence gate, then requires the file to exist
    /// (`VoiceError::ModelMissing` otherwise). Never downloads anything.
    pub fn model_path(
        &self,
        id: &str,
        user_accepted_licence: bool,
        models_dir: &Path,
        ext: &str,
    ) -> Result<std::path::PathBuf, crate::VoiceError> {
        let m = self.get(id, user_accepted_licence)?;
        let path = models_dir.join(format!("{}.{ext}", m.id));
        if path.is_file() {
            Ok(path)
        } else {
            Err(crate::VoiceError::ModelMissing { path })
        }
    }

    /// The default model for a task (always commercial-ok by validation).
    pub fn default_for(&self, task: Task) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.task == task && m.default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(extra: &str) -> String {
        let mut base = serde_json::json!({
            "id":"m","task":"stt","size":"1","licence":"MIT","commercial_ok":true,
            "bundled":false,"url":"TODO","sha256":"TODO","languages":["en"],"default":true
        });
        let over: serde_json::Value = serde_json::from_str(extra).unwrap();
        for (k, v) in over.as_object().unwrap() {
            if v.is_null() {
                base.as_object_mut().unwrap().remove(k);
            } else {
                base[k] = v.clone();
            }
        }
        format!(r#"{{"schema_version":1,"models":[{base}]}}"#)
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
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/registry.json");
        let r = Registry::load(&p).unwrap();
        assert!(r.models.iter().all(|m| !m.bundled));
        for t in [Task::Stt, Task::Tts, Task::Vad] {
            assert!(r.default_for(t).unwrap().commercial_ok);
        }
        for id in ["phonon-40m", "fish-s2-pro"] {
            let m = r.models.iter().find(|m| m.id == id).unwrap();
            assert!(!m.commercial_ok && !m.default);
            assert!(matches!(
                r.get(id, false),
                Err(RegistryError::LicenceNotAccepted(_))
            ));
            assert!(r.get(id, true).is_ok());
        }
        assert!(r.get("phonon-31m", false).is_ok());
    }

    #[test]
    fn model_path_missing_and_present() {
        let r = Registry::from_json_str(&entry("{}")).unwrap();
        let dir = std::env::temp_dir().join(format!("hb-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = r.model_path("m", false, &dir, "bin").unwrap_err();
        assert!(
            matches!(e, crate::VoiceError::ModelMissing { ref path } if path == &dir.join("m.bin"))
        );
        std::fs::write(dir.join("m.bin"), b"x").unwrap();
        assert_eq!(
            r.model_path("m", false, &dir, "bin").unwrap(),
            dir.join("m.bin")
        );
        assert!(matches!(
            r.model_path("nope", false, &dir, "bin"),
            Err(crate::VoiceError::Backend(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_commercial_default_rejected() {
        let e = Registry::from_json_str(&entry(r#"{"commercial_ok":false}"#)).unwrap_err();
        assert!(matches!(e, RegistryError::NonCommercialDefault(_)));
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
    }

    #[test]
    fn bundled_true_rejected() {
        let e = Registry::from_json_str(&entry(r#"{"bundled":true}"#)).unwrap_err();
        assert!(matches!(e, RegistryError::Bundled(_)));
    }

    #[test]
    fn licence_not_accepted_error() {
        let r =
            Registry::from_json_str(&entry(r#"{"commercial_ok":false,"default":false}"#)).unwrap();
        assert!(matches!(
            r.get("m", false),
            Err(RegistryError::LicenceNotAccepted(_))
        ));
        assert!(r.get("m", true).is_ok());
    }
}
