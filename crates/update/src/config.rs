//! Build-time trust configuration and the URL allowlist rule.

use crate::version::Channel;

/// One embedded ed25519 public key.
#[derive(Clone, Copy)]
pub struct TrustedKey {
    pub id: &'static str,
    pub public: [u8; 32],
}

impl std::fmt::Debug for TrustedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TrustedKey({})", self.id)
    }
}

/// Everything the verifier trusts. Constants in the real build; tests build their own with a key
/// generated at runtime.
#[derive(Clone, Copy, Debug)]
pub struct TrustConfig {
    /// The one URL the check may GET.
    pub manifest_url: &'static str,
    /// Exact host names (lowercase ASCII) allowed for the manifest, the artifact and redirects.
    pub hosts: &'static [&'static str],
    pub product: &'static str,
    pub channel: Channel,
    pub keys: &'static [TrustedKey],
}

#[derive(Clone, Copy, Debug)]
pub enum UpdateConfig {
    /// No trusted key compiled in: the app never makes an update network call.
    Unconfigured,
    Configured(&'static TrustConfig),
}

/// The embedded public keys. TODO(S3-7, BLOCKED-ON-USER): the maintainer generates two offline
/// ed25519 keys (primary + backup) with `tools/update-sign keygen` and pastes the PUBLIC halves
/// here, each labelled with its id. Until then this stays empty and updates are "not configured".
pub const TRUSTED_KEYS: &[TrustedKey] = &[];

/// TODO(S3-7): replace `OWNER_TODO` with the real GitHub owner when the keys are added.
static EMBEDDED: TrustConfig = TrustConfig {
    manifest_url:
        "https://github.com/OWNER_TODO/hey_buddy/releases/latest/download/update-manifest.json",
    // VERIFY the current GitHub CDN host on a real release (ADR 0005 section 2a).
    hosts: &[
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
    ],
    product: "dev.heybuddy.desktop",
    channel: Channel::Stable,
    keys: TRUSTED_KEYS,
};

/// `Unconfigured` while [`TRUSTED_KEYS`] is empty.
pub fn embedded() -> UpdateConfig {
    if EMBEDDED.keys.is_empty() {
        UpdateConfig::Unconfigured
    } else {
        UpdateConfig::Configured(&EMBEDDED)
    }
}

/// `https://<host>/path...` where `<host>` equals one of `hosts` (ASCII case-insensitive, exact:
/// no sub-domain, trailing dot, userinfo, port, IPv6 literal, percent escape or non-ASCII
/// look-alike), no backslash, no whitespace / control / non-ASCII byte anywhere, at most 2048
/// bytes. Used for the manifest's artifact URLs and by the redirect policy.
pub fn url_allowed(url: &str, hosts: &[&str]) -> bool {
    if url.len() > 2048 || !url.bytes().all(|b| (0x21..=0x7e).contains(&b)) || url.contains('\\') {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some(end) = rest.find(['/', '?', '#']) else {
        return false;
    };
    if rest.as_bytes()[end] != b'/' {
        return false;
    }
    let host = &rest[..end];
    if host.is_empty() || host.contains(['@', ':', '[', ']', '%']) {
        return false;
    }
    hosts.iter().any(|h| host.eq_ignore_ascii_case(h))
}
