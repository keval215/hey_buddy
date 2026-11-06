//! `verify`: signature first, payload parsing second (ADR 0005 sections 4 and 6).

use std::fmt;

use serde::Deserialize;

use crate::config::{url_allowed, TrustConfig};
use crate::dsse::{b64_decode, ed25519_ok, pae, parse_envelope, parse_json, Envelope};
use crate::version::{Channel, Platform, Version};
use crate::{
    UpdateError, ENDORSEMENT_PAYLOAD_TYPE, MANIFEST_PAYLOAD_TYPE, MAX_ARTIFACT_BYTES,
    MAX_CLOCK_SKEW_SECS, MAX_ENVELOPE_BYTES, MAX_VALIDITY_SECS,
};

const MAX_ARTIFACTS: usize = 8;
const MAX_REVOKED: usize = 64;
const MAX_NOTES_CHARS: usize = 2000;

/// An installer the signed manifest describes. Sealed: only `verify` creates one.
pub struct Artifact {
    url: String,
    sha256: [u8; 32],
    size: u64,
}

impl Artifact {
    /// Passed the host allowlist at verification time.
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl fmt::Debug for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Artifact({} bytes)", self.size)
    }
}

/// A manifest whose signature, product, channel, validity and URLs all passed. Sealed: private
/// fields, only [`verify`] constructs it. `Debug` prints the version and serial only.
pub struct VerifiedManifest {
    version: Version,
    serial: u64,
    issued_at: u64,
    expires_at: u64,
    min_supported: Version,
    revoked: Vec<Version>,
    notes: String,
    artifacts: Vec<(Platform, Artifact)>,
}

impl VerifiedManifest {
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn serial(&self) -> u64 {
        self.serial
    }
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    /// Plain text, control and format characters removed, at most 2000 characters. Show as text.
    pub fn notes(&self) -> &str {
        &self.notes
    }
    pub fn artifact(&self, p: Platform) -> Option<&Artifact> {
        self.artifacts.iter().find(|(q, _)| *q == p).map(|(_, a)| a)
    }
    pub fn min_supported(&self) -> Version {
        self.min_supported
    }
    pub fn is_revoked(&self, v: Version) -> bool {
        self.revoked.contains(&v)
    }
}

impl fmt::Debug for VerifiedManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VerifiedManifest(v{} serial {})",
            self.version, self.serial
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArtifact {
    platform: String,
    kind: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPayload {
    schema: u32,
    product: String,
    channel: String,
    serial: u64,
    version: String,
    issued_at: u64,
    expires_at: u64,
    min_supported: String,
    revoked: Vec<String>,
    notes: String,
    artifacts: Vec<RawArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEndorsement {
    keyid: String,
    public_key: String,
    channel: String,
    not_before_serial: u64,
    not_after_serial: u64,
}

struct Endorsed {
    keyid: String,
    public: [u8; 32],
    not_before: u64,
    not_after: u64,
}

enum Signer {
    Embedded,
    /// Accepted only if the payload's serial lies in this inclusive range.
    Endorsed {
        not_before: u64,
        not_after: u64,
    },
}

/// Endorsements that a trusted key signed (depth 1). Anything wrong with one endorsement just
/// drops it: it is an optional bridge, never a reason to accept something.
fn endorsements(env: &Envelope, cfg: &TrustConfig) -> Vec<Endorsed> {
    let mut out = Vec::new();
    for e in &env.endorsements {
        if e.payload_type != ENDORSEMENT_PAYLOAD_TYPE {
            continue;
        }
        let msg = pae(&e.payload_type, &e.payload);
        let signed = e.signatures.iter().any(|s| {
            cfg.keys
                .iter()
                .find(|k| k.id == s.keyid)
                .is_some_and(|k| ed25519_ok(&k.public, &msg, &s.sig))
        });
        if !signed {
            continue;
        }
        let Ok(raw) = parse_json::<RawEndorsement>(&e.payload) else {
            continue;
        };
        let Ok(public) = b64_decode(&raw.public_key)
            .and_then(|b| <[u8; 32]>::try_from(b).map_err(|_| UpdateError::Malformed))
        else {
            continue;
        };
        if Channel::parse(&raw.channel) != Some(cfg.channel)
            || raw.not_before_serial > raw.not_after_serial
            // An endorsed key may never shadow an embedded id.
            || cfg.keys.iter().any(|k| k.id == raw.keyid)
        {
            continue;
        }
        out.push(Endorsed {
            keyid: raw.keyid,
            public,
            not_before: raw.not_before_serial,
            not_after: raw.not_after_serial,
        });
    }
    out
}

fn find_signer(env: &Envelope, msg: &[u8], cfg: &TrustConfig) -> Result<Signer, UpdateError> {
    // The first signature by an embedded key decides: a bad one is never "skipped".
    for s in &env.signatures {
        if let Some(k) = cfg.keys.iter().find(|k| k.id == s.keyid) {
            return if ed25519_ok(&k.public, msg, &s.sig) {
                Ok(Signer::Embedded)
            } else {
                Err(UpdateError::BadSignature)
            };
        }
    }
    if !env.endorsements.is_empty() {
        let ends = endorsements(env, cfg);
        for s in &env.signatures {
            if let Some(e) = ends.iter().find(|e| e.keyid == s.keyid) {
                return if ed25519_ok(&e.public, msg, &s.sig) {
                    Ok(Signer::Endorsed {
                        not_before: e.not_before,
                        not_after: e.not_after,
                    })
                } else {
                    Err(UpdateError::BadSignature)
                };
            }
        }
    }
    Err(UpdateError::NoTrustedSignature)
}

fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Plain text only: control and format characters go (a newline stays), tabs become spaces.
fn sanitise_notes(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() || is_format_char(c) => None,
            c => Some(c),
        })
        .collect()
}

fn hex32(s: &str) -> Result<[u8; 32], UpdateError> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(UpdateError::Malformed);
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| UpdateError::Malformed)?;
    }
    Ok(out)
}

/// Verify `envelope` (exact bytes from the release host) against `cfg` at `now_unix`.
///
/// Order: size cap, strict envelope parse, payload type, signature (embedded key, or an endorsed
/// key), and ONLY THEN the payload parse and every rule of ADR 0005 section 4.2.
pub fn verify(
    envelope: &[u8],
    cfg: &TrustConfig,
    now_unix: u64,
) -> Result<VerifiedManifest, UpdateError> {
    if envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(UpdateError::TooLarge);
    }
    if cfg.keys.is_empty() {
        return Err(UpdateError::NotConfigured);
    }
    let env = parse_envelope(envelope)?;
    // A signature over another payload type (an endorsement, say) can never validate a manifest:
    // the type is part of the signed message, and a wrong type is refused outright.
    if env.payload_type != MANIFEST_PAYLOAD_TYPE {
        return Err(UpdateError::BadSignature);
    }
    let msg = pae(&env.payload_type, &env.payload);
    let signer = find_signer(&env, &msg, cfg)?;

    // From here the payload is authentic; parse it strictly.
    let raw: RawPayload = parse_json(&env.payload)?;
    if raw.schema != 1 {
        return Err(UpdateError::Malformed);
    }
    if raw.product != cfg.product {
        return Err(UpdateError::WrongProduct);
    }
    if Channel::parse(&raw.channel) != Some(cfg.channel) {
        return Err(UpdateError::WrongChannel);
    }
    if let Signer::Endorsed {
        not_before,
        not_after,
    } = signer
    {
        if raw.serial < not_before || raw.serial > not_after {
            return Err(UpdateError::NoTrustedSignature);
        }
    }
    if raw.artifacts.len() > MAX_ARTIFACTS || raw.revoked.len() > MAX_REVOKED {
        return Err(UpdateError::TooLarge);
    }
    if raw.notes.chars().count() > MAX_NOTES_CHARS {
        return Err(UpdateError::TooLarge);
    }
    let version = Version::parse(&raw.version)?;
    let min_supported = Version::parse(&raw.min_supported)?;
    let revoked = raw
        .revoked
        .iter()
        .map(|v| Version::parse(v))
        .collect::<Result<Vec<_>, _>>()?;
    if raw.expires_at < raw.issued_at {
        return Err(UpdateError::Malformed);
    }
    if raw.expires_at - raw.issued_at > MAX_VALIDITY_SECS {
        return Err(UpdateError::ValidityTooLong);
    }
    if raw.issued_at > now_unix.saturating_add(MAX_CLOCK_SKEW_SECS) {
        return Err(UpdateError::ClockSkew);
    }
    if now_unix > raw.expires_at {
        return Err(UpdateError::Expired);
    }
    let mut artifacts: Vec<(Platform, Artifact)> = Vec::new();
    for a in raw.artifacts {
        // A platform this build does not know is ignored (a later release may add one).
        let Some(platform) = Platform::parse(&a.platform) else {
            continue;
        };
        if a.kind != platform.kind() || artifacts.iter().any(|(p, _)| *p == platform) {
            return Err(UpdateError::Malformed);
        }
        if a.size == 0 {
            return Err(UpdateError::Malformed);
        }
        if a.size > MAX_ARTIFACT_BYTES {
            return Err(UpdateError::TooLarge);
        }
        if !url_allowed(&a.url, cfg.hosts) {
            return Err(UpdateError::BadUrl);
        }
        artifacts.push((
            platform,
            Artifact {
                url: a.url,
                sha256: hex32(&a.sha256)?,
                size: a.size,
            },
        ));
    }
    Ok(VerifiedManifest {
        version,
        serial: raw.serial,
        issued_at: raw.issued_at,
        expires_at: raw.expires_at,
        min_supported,
        revoked,
        notes: sanitise_notes(&raw.notes),
        artifacts,
    })
}
