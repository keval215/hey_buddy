use thiserror::Error;

/// Every failure of this crate (ADR 0002 section 9).
///
/// Messages are fixed strings or short allowlisted tokens. They NEVER contain response bodies,
/// transport error text, transcripts, replies, screen text or keys.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LlmError {
    /// A cloud call was attempted without a valid, matching [`crate::CloudOptIn`].
    #[error("cloud call not opted in for this exact request")]
    NotOptedIn,
    /// No API key is stored.
    #[error("no credentials stored")]
    NoCredentials,
    /// 401 / 403.
    #[error("authentication failed")]
    Auth,
    /// 429.
    #[error("rate limited")]
    RateLimited,
    #[error("timed out")]
    Timeout,
    /// Any other non-2xx status.
    #[error("http status {status}")]
    Http { status: u16 },
    #[error("network error: {0}")]
    Network(&'static str),
    /// Provider-reported error: an allowlisted token (e.g. `overloaded_error`) or `unknown`.
    #[error("provider error: {0}")]
    Provider(String),
    #[error("protocol error: {0}")]
    Protocol(&'static str),
    #[error("encode error: {0}")]
    Encode(&'static str),
    #[error("configuration error: {0}")]
    Config(&'static str),
    #[error("cancelled")]
    Cancelled,
}
