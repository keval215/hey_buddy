//! Errors of the core wiring. Fixed tokens only: no transcripts, screen text, keys or paths.

use llm::LlmError;

/// Why a turn could not get a request approved or prepared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// A cloud provider was requested while `local_only` is on (or the gate saw `local_only`).
    #[error("local_only")]
    LocalOnly,
    /// The user cancelled, the preview timed out, the turn ended, or no preview could be shown.
    #[error("cancelled")]
    Cancelled,
    /// The provider failed to prepare the request.
    #[error("llm: {0}")]
    Llm(#[from] LlmError),
}

/// Failures of `set_api_key` / `clear_api_key`. `Display` is the fixed token shown to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("unsupported_provider")]
    UnsupportedProvider,
    #[error("invalid_key")]
    InvalidKey,
    #[error("credential_store_unsupported")]
    StoreUnsupported,
    #[error("keychain_error")]
    Keychain,
}

impl From<LlmError> for KeyError {
    fn from(e: LlmError) -> Self {
        match e {
            LlmError::Config("credential_store_unsupported") => KeyError::StoreUnsupported,
            _ => KeyError::Keychain,
        }
    }
}
