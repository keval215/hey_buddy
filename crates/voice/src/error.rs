use crate::registry::RegistryError;
use std::path::PathBuf;

/// Errors from voice components. Messages never include transcript or audio content.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("model file missing: {}", path.display())]
    ModelMissing { path: PathBuf },
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    /// The message must describe the problem only (e.g. lengths, rates), never audio content.
    #[error("invalid audio: {0}")]
    InvalidAudio(String),
    /// Wrap backend errors with a fixed description; never include transcript text,
    /// audio samples or other user content in the message.
    #[error("backend error: {0}")]
    Backend(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<RegistryError> for VoiceError {
    fn from(e: RegistryError) -> Self {
        VoiceError::Backend(e.to_string())
    }
}
