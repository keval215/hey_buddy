//! hey_buddy `llm` crate (ADR 0002, decision D-003).
//!
//! A neutral [`LlmRequest`] becomes a [`PreparedRequest`] through [`LlmProvider::prepare`]:
//! the exact HTTP body bytes, endpoint, model, provider id, locality and a digest. The
//! preview ([`RequestPreview`]), the digest, the cloud opt-in ([`CloudOptIn`]) and the
//! transport all use that one object, so previewed bytes == sent bytes.
//!
//! Screen content enters only as [`firewall::RedactedFrame`] / [`firewall::RedactedScreenText`];
//! there is no API taking `frame::Frame`, raw image bytes or raw tree text.
//!
//! Local first: [`OllamaProvider`] accepts loopback base URLs only. Cloud calls need a
//! per-request, non-cloneable [`CloudOptIn`] minted from a preview "Send"
//! (`crates/core/src/consent.rs`; CI enforces that nothing else calls
//! `CloudOptIn::for_request`).
//!
//! # Features
//! - none: types, builders, parsers, `prepare`, providers with an injected transport.
//! - `ollama` / `anthropic`: `ReqwestTransport` (reqwest + rustls with the `ring` provider).
//! - `keychain`: `KeyringStore` on macOS / Windows only (no-op on Linux).
//! - `fakes`: `FakeLlm`, `FakeTransport`, `FakeCredentialStore` (dev-dependencies only, L-006).
//!
//! # Manual tests (not run in CI)
//! `cargo test -p llm --features ollama -- --ignored ollama_real` talks to a real Ollama on
//! 127.0.0.1:11434 (`ollama pull qwen3:4b` first). `cargo test -p llm --features anthropic
//! -- --ignored anthropic_real` needs a key stored in the OS keychain (macOS / Windows) and
//! the model id in the environment variable `HEY_BUDDY_ANTHROPIC_MODEL`; it sends one tiny
//! request to the real API.
//!
//! Errors, `Debug` output and logs never contain transcripts, replies, screen text, response
//! bodies, transport error text or keys.

mod anthropic;
mod credential;
mod error;
mod image;
mod ollama;
mod optin;
mod provider;
mod request;
mod stream;
mod transport;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

pub use anthropic::{
    AnthropicConfig, AnthropicProvider, ANTHROPIC_ENDPOINT, ANTHROPIC_PROVIDER_ID,
};
#[cfg(all(feature = "keychain", any(target_os = "macos", target_os = "windows")))]
pub use credential::KeyringStore;
pub use credential::{
    default_credential_store, CredentialStore, UnsupportedCredentialStore, ANTHROPIC_ACCOUNT,
    KEYCHAIN_SERVICE,
};
pub use error::LlmError;
pub use image::{ImageGeometry, MAX_LONG_EDGE, MAX_PIXELS, MAX_PNG_BYTES};
pub use ollama::{
    validate_loopback_base_url, OllamaConfig, OllamaProvider, DEFAULT_OLLAMA_BASE_URL,
    DEFAULT_TEXT_MODEL, DEFAULT_VISION_MODEL, OLLAMA_PROVIDER_ID,
};
pub use optin::{CloudOptIn, ConsentSource};
pub use provider::LlmProvider;
pub use request::{
    LlmRequest, Locality, PreparedRequest, RequestPreview, ScreenContext, DIGEST_DOMAIN,
};
pub use stream::{AnthropicSseParser, LlmDelta, OllamaNdjsonParser, StopReason};
#[cfg(any(feature = "ollama", feature = "anthropic"))]
pub use transport::ReqwestTransport;
pub use transport::{HeaderValue, HttpResponse, HttpTransport, Timeouts};
