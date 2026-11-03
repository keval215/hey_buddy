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

// ---------------------------------------------------------------- turn:error mapping

use crate::api::TurnError;

fn te(code: &str, message: &str) -> TurnError {
    TurnError {
        code: code.into(),
        message: message.into(),
    }
}

/// `turn:error` for an LLM failure. Fixed code and message per variant; the payload of the
/// error (status codes, provider tokens) is deliberately left out.
pub fn llm_turn_error(e: &LlmError) -> TurnError {
    match e {
        LlmError::NotOptedIn => te("llm_not_opted_in", "The request was not approved."),
        LlmError::NoCredentials => te("llm_no_credentials", "No API key is stored."),
        LlmError::Auth => te("llm_auth", "The language model rejected the API key."),
        LlmError::RateLimited => te("llm_rate_limited", "The language model is rate limited."),
        LlmError::Timeout => te("llm_timeout", "The language model took too long."),
        LlmError::Http { .. } => te("llm_http", "The language model returned an error."),
        LlmError::Network(_) => te(
            "llm_network",
            "Could not reach the language model. Is Ollama running?",
        ),
        LlmError::Provider(_) => te("llm_provider", "The language model reported an error."),
        LlmError::Protocol(_) => te(
            "llm_protocol",
            "The language model sent an unexpected reply.",
        ),
        LlmError::Encode(_) => te("llm_encode", "The request could not be built."),
        LlmError::Config(_) => te("llm_config", "The language model is not configured."),
        LlmError::Cancelled => te("cancelled", "Cancelled."),
    }
}

/// `turn:error` for a voice failure. `ModelMissing` names the expected file (a path, never
/// user content) so the user knows what to place where.
pub fn voice_turn_error(e: &voice::VoiceError) -> Option<TurnError> {
    use voice::VoiceError as V;
    Some(match e {
        V::ModelMissing { path } => te(
            "model_missing",
            &format!("A voice model file is missing: {}", path.display()),
        ),
        V::DeviceUnavailable(_) => te(
            "audio_device_unavailable",
            "The microphone or speaker is unavailable.",
        ),
        V::InvalidAudio(_) => te("audio_invalid", "The audio could not be processed."),
        V::Backend(_) => te("voice_failed", "A voice engine failed."),
        V::Cancelled => return None,
    })
}

pub fn simple_turn_error(code: &str, message: &str) -> TurnError {
    te(code, message)
}
