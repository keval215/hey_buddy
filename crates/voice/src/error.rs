use crate::registry::RegistryError;
use std::path::PathBuf;

/// Errors from voice components. Messages never include transcript or audio content.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("model file missing: {}", path.display())]
    ModelMissing { path: PathBuf },
    #[error("licence not accepted for model `{model}`")]
    LicenceNotAccepted { model: String },
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("invalid audio: {0}")]
    InvalidAudio(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<RegistryError> for VoiceError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::LicenceNotAccepted(model) => VoiceError::LicenceNotAccepted { model },
            other => VoiceError::Backend(other.to_string()),
        }
    }
}
