//! The DSSE-style envelope (ADR 0005 section 4.1): strict parsing and the signed message.
//!
//! Strictness: `deny_unknown_fields` on every object, and serde's derived struct deserializer
//! rejects a repeated key (`duplicate field`), so a duplicate key is an error at every level. The
//! base64 fields must be canonical (decode, re-encode, compare) so one signed message has exactly
//! one textual form.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Deserialize;

use crate::UpdateError;

/// Most signatures one envelope may carry.
pub const MAX_SIGNATURES: usize = 4;
/// Most endorsements one manifest envelope may carry.
pub const MAX_ENDORSEMENTS: usize = 2;
/// Longest key id.
pub const MAX_KEYID_CHARS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignature {
    keyid: String,
    sig: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawEnvelope {
    payload_type: String,
    payload: String,
    signatures: Vec<RawSignature>,
    #[serde(default)]
    endorsements: Vec<RawInner>,
}

/// An endorsement is an envelope WITHOUT further endorsements (chain depth 1).
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawInner {
    payload_type: String,
    payload: String,
    signatures: Vec<RawSignature>,
}

pub(crate) struct Signature {
    pub keyid: String,
    pub sig: [u8; 64],
}

/// A parsed, NOT yet verified envelope.
pub(crate) struct Envelope {
    pub payload_type: String,
    pub payload: Vec<u8>,
    pub signatures: Vec<Signature>,
    pub endorsements: Vec<Envelope>,
}

/// Canonical base64 (standard alphabet, padded, nothing else).
pub(crate) fn b64_decode(s: &str) -> Result<Vec<u8>, UpdateError> {
    let bytes = STANDARD.decode(s).map_err(|_| UpdateError::Malformed)?;
    if STANDARD.encode(&bytes) != s {
        return Err(UpdateError::Malformed);
    }
    Ok(bytes)
}

fn json_error(e: &serde_json::Error) -> UpdateError {
    // serde_json has no structured "unknown field" category; the message prefix is fixed text
    // from serde and is never shown to anyone.
    if e.to_string().starts_with("unknown field") {
        UpdateError::UnknownField
    } else {
        UpdateError::Malformed
    }
}

/// Strict JSON parse used for every payload too. Rejects trailing data.
pub(crate) fn parse_json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, UpdateError> {
    serde_json::from_slice(bytes).map_err(|e| json_error(&e))
}

fn convert_sigs(raw: Vec<RawSignature>) -> Result<Vec<Signature>, UpdateError> {
    if raw.len() > MAX_SIGNATURES {
        return Err(UpdateError::TooLarge);
    }
    raw.into_iter()
        .map(|s| {
            if s.keyid.is_empty()
                || s.keyid.chars().count() > MAX_KEYID_CHARS
                || !s.keyid.chars().all(|c| c.is_ascii_graphic())
            {
                return Err(UpdateError::Malformed);
            }
            let sig: [u8; 64] = b64_decode(&s.sig)?
                .try_into()
                .map_err(|_| UpdateError::Malformed)?;
            Ok(Signature {
                keyid: s.keyid,
                sig,
            })
        })
        .collect()
}

fn convert_inner(
    payload_type: String,
    payload: String,
    signatures: Vec<RawSignature>,
) -> Result<Envelope, UpdateError> {
    Ok(Envelope {
        payload_type,
        payload: b64_decode(&payload)?,
        signatures: convert_sigs(signatures)?,
        endorsements: Vec::new(),
    })
}

/// Parse (no signature check). The caller has already applied the overall size cap.
pub(crate) fn parse_envelope(bytes: &[u8]) -> Result<Envelope, UpdateError> {
    let raw: RawEnvelope = parse_json(bytes)?;
    if raw.endorsements.len() > MAX_ENDORSEMENTS {
        return Err(UpdateError::TooLarge);
    }
    let endorsements = raw
        .endorsements
        .into_iter()
        .map(|e| convert_inner(e.payload_type, e.payload, e.signatures))
        .collect::<Result<Vec<_>, _>>()?;
    let mut env = convert_inner(raw.payload_type, raw.payload, raw.signatures)?;
    env.endorsements = endorsements;
    Ok(env)
}

/// The message that is signed: `"DSSEv1" SP len(type) SP type SP len(payload) SP payload`
/// (ASCII decimal lengths).
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + payload_type.len() + 32);
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload);
    out
}

/// ed25519 verify of `sig` over `msg` with the raw 32-byte public key.
pub(crate) fn ed25519_ok(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
        .verify(msg, sig)
        .is_ok()
}
