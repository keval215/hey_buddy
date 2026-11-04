//! Fixed error tokens. `Display` is the token; no input text ever ends up in an error.

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// No trusted key is compiled in (this build cannot check for updates).
    #[error("updates_not_configured")]
    NotConfigured,
    /// Envelope or response over the cap, or a count / size limit exceeded.
    #[error("too_large")]
    TooLarge,
    #[error("malformed")]
    Malformed,
    #[error("unknown_field")]
    UnknownField,
    /// No signature by a trusted (or endorsed) key.
    #[error("no_trusted_signature")]
    NoTrustedSignature,
    /// A known key's signature does not verify.
    #[error("bad_signature")]
    BadSignature,
    #[error("wrong_product")]
    WrongProduct,
    #[error("wrong_channel")]
    WrongChannel,
    /// `issued_at` is more than a day in the future.
    #[error("clock_skew")]
    ClockSkew,
    #[error("manifest_expired")]
    Expired,
    #[error("validity_too_long")]
    ValidityTooLong,
    /// The offered version is older than the running one.
    #[error("stale_manifest")]
    StaleManifest,
    /// The serial is lower than one already seen.
    #[error("replayed_manifest")]
    ReplayedManifest,
    #[error("bad_url")]
    BadUrl,
    /// No artifact for this platform.
    #[error("no_artifact")]
    NoArtifact,
    #[error("bad_version")]
    BadVersion,
    /// The target version is revoked by a verified manifest.
    #[error("revoked")]
    Revoked,
}

impl UpdateError {
    /// The fixed token (same as `Display`).
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "updates_not_configured",
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::UnknownField => "unknown_field",
            Self::NoTrustedSignature => "no_trusted_signature",
            Self::BadSignature => "bad_signature",
            Self::WrongProduct => "wrong_product",
            Self::WrongChannel => "wrong_channel",
            Self::ClockSkew => "clock_skew",
            Self::Expired => "manifest_expired",
            Self::ValidityTooLong => "validity_too_long",
            Self::StaleManifest => "stale_manifest",
            Self::ReplayedManifest => "replayed_manifest",
            Self::BadUrl => "bad_url",
            Self::NoArtifact => "no_artifact",
            Self::BadVersion => "bad_version",
            Self::Revoked => "revoked",
        }
    }
}
