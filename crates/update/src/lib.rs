//! Update manifest verifier and installer launch (ADR 0005, ticket S3-3).
//!
//! PURE and SYNC: no network, no async, no clock (callers pass `now_unix`), no file IO outside
//! the `cfg(windows)` launcher. The HTTP check and the download live in `buddy-core::update`.
//!
//! Trust model: the only way to obtain a [`VerifiedManifest`] is [`verify`], which checks an
//! ed25519 signature (DSSE-style envelope, see [`mod@dsse`]) made by an embedded key, or by a key
//! that one embedded key endorsed (depth 1), BEFORE the payload JSON is parsed. Every failure is
//! a fixed token ([`UpdateError::code`]); nothing from the envelope reaches an error or a log.
//!
//! `TRUSTED_KEYS` is EMPTY until the maintainer generates the offline keys (S3-7), so
//! [`embedded`] returns [`UpdateConfig::Unconfigured`] and the app never contacts any host.
//!
//! The sealed types cannot be built outside [`verify`] (private fields, no constructor). Newer
//! rustc reports a struct literal on private fields without an error code, so the pinned-code
//! snippets (checked by `tools/check-doctest-codes.sh`) read a private field instead:
//!
//! ```compile_fail
//! // No struct literal outside the verifier.
//! let _m = update::VerifiedManifest {
//!     version: todo!(),
//!     serial: 1,
//! };
//! ```
//!
//! ```compile_fail,E0616
//! // The fields are private: a caller can only use the accessors.
//! fn f(m: &update::VerifiedManifest) -> u64 {
//!     m.serial
//! }
//! ```
//!
//! ```compile_fail,E0616
//! fn f(a: &update::Artifact) -> u64 {
//!     a.size
//! }
//! ```
//!
//! There is no public constructor for the staged installer on any platform but through `stage`
//! (Windows only), so a launcher can only run a file that was hashed against a verified artifact.

mod config;
mod decide;
pub mod dsse;
mod error;
pub mod launch;
mod verify;
mod version;

pub use config::{embedded, url_allowed, TrustConfig, TrustedKey, UpdateConfig, TRUSTED_KEYS};
pub use decide::{decide, Advice, Context, Offer};
pub use error::UpdateError;
pub use verify::{verify, Artifact, VerifiedManifest};
pub use version::{Channel, Platform, Version};

/// Largest envelope accepted (bytes). Enforced on the buffer and, in core, while streaming.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
/// Largest installer accepted (bytes).
pub const MAX_ARTIFACT_BYTES: u64 = 512 << 20;
/// Longest allowed `expires_at - issued_at`.
pub const MAX_VALIDITY_SECS: u64 = 180 * 86_400;
/// How far in the future `issued_at` may be.
pub const MAX_CLOCK_SKEW_SECS: u64 = 86_400;
/// `payloadType` of the update manifest envelope (domain separation).
pub const MANIFEST_PAYLOAD_TYPE: &str = "application/vnd.heybuddy.update-manifest+json; v=1";
/// `payloadType` of a key endorsement envelope.
pub const ENDORSEMENT_PAYLOAD_TYPE: &str = "application/vnd.heybuddy.key-endorsement+json; v=1";

#[cfg(test)]
mod tests;
