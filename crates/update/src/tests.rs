//! Verifier tests (ADR 0005 section 11). Keys are generated at runtime, in this `cfg(test)`
//! module only; nothing private is ever committed.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};

use crate::dsse::pae;
use crate::launch::{
    command_line, is_run_dir_name, run_dir_name, setup_path, LaunchError, PRIVATE_DIR_SDDL,
    SETUP_FILE_NAME,
};
use crate::*;

const NOW: u64 = 1_793_800_000;
const ISSUED: u64 = 1_793_750_400;
const EXPIRES: u64 = ISSUED + 90 * 86_400;
const HOSTS: &[&str] = &["github.com", "release-assets.githubusercontent.com"];
const GOOD_URL: &str = "https://github.com/o/hey_buddy/releases/download/v0.2.0/setup.exe";
const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn keypair() -> Ed25519KeyPair {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap()
}

fn public(kp: &Ed25519KeyPair) -> [u8; 32] {
    kp.public_key().as_ref().try_into().unwrap()
}

fn config(keys: &[(&'static str, [u8; 32])]) -> &'static TrustConfig {
    let keys: Vec<TrustedKey> = keys
        .iter()
        .map(|(id, public)| TrustedKey {
            id,
            public: *public,
        })
        .collect();
    Box::leak(Box::new(TrustConfig {
        manifest_url:
            "https://github.com/o/hey_buddy/releases/latest/download/update-manifest.json",
        hosts: HOSTS,
        product: "dev.heybuddy.desktop",
        channel: Channel::Stable,
        keys: Box::leak(keys.into_boxed_slice()),
    }))
}

fn payload(serial: u64, version: &str) -> Value {
    json!({
        "schema": 1,
        "product": "dev.heybuddy.desktop",
        "channel": "stable",
        "serial": serial,
        "version": version,
        "issued_at": ISSUED,
        "expires_at": EXPIRES,
        "min_supported": "0.1.0",
        "revoked": ["0.1.1"],
        "notes": "Fixes things.",
        "artifacts": [{
            "platform": "windows-x86_64", "kind": "nsis",
            "url": GOOD_URL, "sha256": SHA, "size": 7_340_032u64
        }]
    })
}

fn b64(b: &[u8]) -> String {
    STANDARD.encode(b)
}

fn sig_obj(kp: &Ed25519KeyPair, keyid: &str, ptype: &str, payload: &[u8]) -> Value {
    json!({"keyid": keyid, "sig": b64(kp.sign(&pae(ptype, payload)).as_ref())})
}

fn envelope_with(kp: &Ed25519KeyPair, keyid: &str, ptype: &str, payload: &[u8]) -> Value {
    json!({
        "payloadType": ptype,
        "payload": b64(payload),
        "signatures": [sig_obj(kp, keyid, ptype, payload)],
    })
}

fn signed(kp: &Ed25519KeyPair, keyid: &str, p: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(p).unwrap();
    serde_json::to_vec(&envelope_with(kp, keyid, MANIFEST_PAYLOAD_TYPE, &bytes)).unwrap()
}

struct Fx {
    kp: Ed25519KeyPair,
    cfg: &'static TrustConfig,
}

fn fx() -> Fx {
    let kp = keypair();
    let cfg = config(&[("hb-test-a", public(&kp))]);
    Fx { kp, cfg }
}

impl Fx {
    fn env(&self, p: &Value) -> Vec<u8> {
        signed(&self.kp, "hb-test-a", p)
    }
    fn verify(&self, p: &Value) -> Result<VerifiedManifest, UpdateError> {
        verify(&self.env(p), self.cfg, NOW)
    }
    fn verify_err(&self, p: &Value) -> UpdateError {
        self.verify(p).unwrap_err()
    }
}

fn ctx(current: &str, highest_serial: u64) -> Context {
    Context {
        current: Version::parse(current).unwrap(),
        platform: Platform::WindowsX86_64,
        highest_serial,
        now_unix: NOW,
    }
}

// ---- happy path ----------------------------------------------------------------------------

#[test]
fn valid_envelope_verifies_and_decides_available() {
    let f = fx();
    let m = f.verify(&payload(12, "0.2.0")).unwrap();
    assert_eq!(m.version(), Version::parse("0.2.0").unwrap());
    assert_eq!(m.serial(), 12);
    assert_eq!(m.expires_at(), EXPIRES);
    assert_eq!(m.notes(), "Fixes things.");
    assert_eq!(m.min_supported(), Version::parse("0.1.0").unwrap());
    let a = m.artifact(Platform::WindowsX86_64).unwrap();
    assert_eq!(a.url(), GOOD_URL);
    assert_eq!(a.size(), 7_340_032);
    assert_eq!(a.sha256()[0], 0x01);
    assert_eq!(a.sha256()[31], 0xef);
    let adv = decide(&m, &ctx("0.1.0", 11)).unwrap();
    assert_eq!(
        adv.offer,
        Offer::Available {
            version: Version::parse("0.2.0").unwrap(),
            size: 7_340_032
        }
    );
    assert!(!adv.current_revoked && !adv.below_min_supported);
}

#[test]
fn debug_prints_version_and_serial_only() {
    let f = fx();
    let mut p = payload(12, "0.2.0");
    p["notes"] = json!("SECRET-NOTE-TEXT");
    let m = f.verify(&p).unwrap();
    let d = format!("{m:?}");
    assert_eq!(d, "VerifiedManifest(v0.2.0 serial 12)");
    assert!(!d.contains("SECRET") && !d.contains("github"));
    let a = format!("{:?}", m.artifact(Platform::WindowsX86_64).unwrap());
    assert!(!a.contains("github") && !a.contains("0123"));
}

#[test]
fn boundary_times_are_accepted() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["issued_at"] = json!(NOW + 86_400);
    p["expires_at"] = json!(NOW + 86_400 + MAX_VALIDITY_SECS);
    assert!(f.verify(&p).is_ok());
    let mut p = payload(1, "0.2.0");
    p["expires_at"] = json!(NOW);
    assert!(f.verify(&p).is_ok());
}

// ---- signatures ----------------------------------------------------------------------------

#[test]
fn wrong_key_with_a_known_keyid_is_bad_signature() {
    let f = fx();
    let other = keypair();
    let env = signed(&other, "hb-test-a", &payload(1, "0.2.0"));
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

#[test]
fn unknown_keyid_only_is_no_trusted_signature() {
    let f = fx();
    let env = signed(&f.kp, "hb-unknown", &payload(1, "0.2.0"));
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
}

#[test]
fn no_signatures_is_no_trusted_signature() {
    let f = fx();
    let bytes = serde_json::to_vec(&payload(1, "0.2.0")).unwrap();
    let env =
        json!({"payloadType": MANIFEST_PAYLOAD_TYPE, "payload": b64(&bytes), "signatures": []});
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
}

#[test]
fn payload_tampered_after_signing_is_bad_signature() {
    let f = fx();
    let p = payload(1, "0.2.0");
    let mut env: Value = serde_json::from_slice(&f.env(&p)).unwrap();
    let mut evil = payload(1, "0.2.0");
    evil["artifacts"][0]["sha256"] = json!("f".repeat(64));
    env["payload"] = json!(b64(&serde_json::to_vec(&evil).unwrap()));
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

#[test]
fn swapped_payload_type_is_bad_signature() {
    let f = fx();
    let mut env: Value = serde_json::from_slice(&f.env(&payload(1, "0.2.0"))).unwrap();
    env["payloadType"] = json!(ENDORSEMENT_PAYLOAD_TYPE);
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

#[test]
fn a_validly_signed_endorsement_is_not_a_manifest() {
    let f = fx();
    let end = serde_json::to_vec(&json!({
        "keyid": "hb-new", "public_key": b64(&[7u8; 32]), "channel": "stable",
        "not_before_serial": 1, "not_after_serial": 9
    }))
    .unwrap();
    let env = envelope_with(&f.kp, "hb-test-a", ENDORSEMENT_PAYLOAD_TYPE, &end);
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

#[test]
fn a_bad_signature_by_a_known_key_is_not_skipped() {
    let f = fx();
    let bytes = serde_json::to_vec(&payload(1, "0.2.0")).unwrap();
    let other = keypair();
    let env = json!({
        "payloadType": MANIFEST_PAYLOAD_TYPE, "payload": b64(&bytes),
        "signatures": [
            sig_obj(&other, "hb-test-a", MANIFEST_PAYLOAD_TYPE, &bytes),
            sig_obj(&f.kp, "hb-test-a", MANIFEST_PAYLOAD_TYPE, &bytes),
        ]
    });
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

#[test]
fn an_unknown_keyid_is_skipped_when_a_known_key_also_signed() {
    let f = fx();
    let bytes = serde_json::to_vec(&payload(1, "0.2.0")).unwrap();
    let env = json!({
        "payloadType": MANIFEST_PAYLOAD_TYPE, "payload": b64(&bytes),
        "signatures": [
            sig_obj(&f.kp, "hb-other", MANIFEST_PAYLOAD_TYPE, &bytes),
            sig_obj(&f.kp, "hb-test-a", MANIFEST_PAYLOAD_TYPE, &bytes),
        ]
    });
    assert!(verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).is_ok());
}

#[test]
fn a_second_trusted_key_also_works() {
    let a = keypair();
    let b = keypair();
    let cfg = config(&[("hb-a", public(&a)), ("hb-b", public(&b))]);
    assert!(verify(&signed(&b, "hb-b", &payload(1, "0.2.0")), cfg, NOW).is_ok());
    assert!(verify(&signed(&a, "hb-a", &payload(1, "0.2.0")), cfg, NOW).is_ok());
}

#[test]
fn more_than_four_signatures_is_too_large() {
    let f = fx();
    let bytes = serde_json::to_vec(&payload(1, "0.2.0")).unwrap();
    let s = sig_obj(&f.kp, "hb-test-a", MANIFEST_PAYLOAD_TYPE, &bytes);
    let env = json!({
        "payloadType": MANIFEST_PAYLOAD_TYPE, "payload": b64(&bytes),
        "signatures": [s.clone(), s.clone(), s.clone(), s.clone(), s]
    });
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::TooLarge
    );
}

#[test]
fn pae_is_the_dsse_v1_message() {
    assert_eq!(
        pae("application/x", b"hello"),
        b"DSSEv1 13 application/x 5 hello".to_vec()
    );
    assert_eq!(pae("", b""), b"DSSEv1 0  0 ".to_vec());
}

// ---- strict envelope parsing ---------------------------------------------------------------

#[test]
fn oversized_envelope_is_too_large_before_parsing() {
    let f = fx();
    let big = vec![b' '; MAX_ENVELOPE_BYTES + 1];
    assert_eq!(verify(&big, f.cfg, NOW).unwrap_err(), UpdateError::TooLarge);
    // Exactly at the cap is parsed (and found malformed here, not too large).
    let at_cap = vec![b' '; MAX_ENVELOPE_BYTES];
    assert_eq!(
        verify(&at_cap, f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    let f = fx();
    let good = f.env(&payload(1, "0.2.0"));
    // Envelope level.
    let mut env: Value = serde_json::from_slice(&good).unwrap();
    env["extra"] = json!(1);
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::UnknownField
    );
    // Signature level.
    let mut env: Value = serde_json::from_slice(&good).unwrap();
    env["signatures"][0]["extra"] = json!(1);
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::UnknownField
    );
    // Payload level (signed, so only the field matters).
    let mut p = payload(1, "0.2.0");
    p["surprise"] = json!(true);
    assert_eq!(f.verify_err(&p), UpdateError::UnknownField);
    // Artifact level.
    let mut p = payload(1, "0.2.0");
    p["artifacts"][0]["args"] = json!("/S");
    assert_eq!(f.verify_err(&p), UpdateError::UnknownField);
}

#[test]
fn missing_fields_are_malformed() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p.as_object_mut().unwrap().remove("revoked");
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
}

#[test]
fn duplicate_keys_are_rejected_at_every_level() {
    let f = fx();
    // Envelope level: two "payloadType" keys.
    let good = String::from_utf8(f.env(&payload(1, "0.2.0"))).unwrap();
    let dup = good.replacen('{', "{\"payloadType\":\"x\",", 1);
    assert_eq!(
        verify(dup.as_bytes(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    // Signature level.
    let dup = good.replacen("\"keyid\"", "\"keyid\":\"a\",\"keyid\"", 1);
    assert_eq!(
        verify(dup.as_bytes(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    // Payload level, signed: the first "serial" would be fine, the second is not.
    let p = serde_json::to_string(&payload(1, "0.2.0")).unwrap();
    let dup_payload = p.replacen('{', "{\"serial\":99,", 1);
    let env = serde_json::to_vec(&envelope_with(
        &f.kp,
        "hb-test-a",
        MANIFEST_PAYLOAD_TYPE,
        dup_payload.as_bytes(),
    ))
    .unwrap();
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    // Artifact level, signed.
    let dup_payload = p.replacen("\"platform\"", "\"platform\":\"x\",\"platform\"", 1);
    let env = serde_json::to_vec(&envelope_with(
        &f.kp,
        "hb-test-a",
        MANIFEST_PAYLOAD_TYPE,
        dup_payload.as_bytes(),
    ))
    .unwrap();
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
}

#[test]
fn non_canonical_base64_is_malformed() {
    let f = fx();
    let good: Value = serde_json::from_slice(&f.env(&payload(1, "0.2.0"))).unwrap();
    // Payload without padding.
    let mut env = good.clone();
    let p = env["payload"]
        .as_str()
        .unwrap()
        .trim_end_matches('=')
        .to_string();
    assert_ne!(
        p,
        good["payload"].as_str().unwrap(),
        "test needs a padded payload"
    );
    env["payload"] = json!(p);
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    // URL-safe alphabet / whitespace in the signature.
    let mut env = good.clone();
    let s = env["signatures"][0]["sig"].as_str().unwrap().to_string();
    env["signatures"][0]["sig"] = json!(format!("{s}\n"));
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    // A 64-byte signature has 2 spare bits in the last base64 digit: non-zero ones are refused.
    let mut env = good.clone();
    let raw = STANDARD
        .decode(env["signatures"][0]["sig"].as_str().unwrap())
        .unwrap();
    let mut s = b64(&raw).into_bytes();
    // 64 bytes => 88 chars with "==" padding: the char before the padding carries 4 spare bits.
    let i = s.len() - 3;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let cur = alphabet.iter().position(|c| *c == s[i]).unwrap();
    s[i] = alphabet[cur ^ 1];
    env["signatures"][0]["sig"] = json!(String::from_utf8(s).unwrap());
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
}

#[test]
fn signature_must_be_exactly_64_bytes() {
    let f = fx();
    let mut env: Value = serde_json::from_slice(&f.env(&payload(1, "0.2.0"))).unwrap();
    env["signatures"][0]["sig"] = json!(b64(&[1u8; 63]));
    assert_eq!(
        verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
}

#[test]
fn keyids_are_short_printable_ascii() {
    let f = fx();
    for bad in ["", "a b", "k\u{e9}y", &"k".repeat(65)] {
        let mut env: Value = serde_json::from_slice(&f.env(&payload(1, "0.2.0"))).unwrap();
        env["signatures"][0]["keyid"] = json!(bad);
        assert_eq!(
            verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).unwrap_err(),
            UpdateError::Malformed,
            "{bad:?}"
        );
    }
}

#[test]
fn truncated_trailing_and_non_json_input_is_malformed() {
    let f = fx();
    let good = f.env(&payload(1, "0.2.0"));
    for cut in [0, 1, good.len() / 2, good.len() - 1] {
        assert_eq!(
            verify(&good[..cut], f.cfg, NOW).unwrap_err(),
            UpdateError::Malformed,
            "cut {cut}"
        );
    }
    let mut trailing = good.clone();
    trailing.extend_from_slice(b" {}");
    assert_eq!(
        verify(&trailing, f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    assert_eq!(
        verify(b"[]", f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
    assert_eq!(
        verify(&[0xff, 0xfe], f.cfg, NOW).unwrap_err(),
        UpdateError::Malformed
    );
}

#[test]
fn unsupported_schema_is_malformed() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["schema"] = json!(2);
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
}

// ---- payload rules -------------------------------------------------------------------------

#[test]
fn wrong_product_and_channel() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["product"] = json!("dev.heybuddy.other");
    assert_eq!(f.verify_err(&p), UpdateError::WrongProduct);
    let mut p = payload(1, "0.2.0");
    p["channel"] = json!("beta");
    assert_eq!(f.verify_err(&p), UpdateError::WrongChannel);
}

#[test]
fn expiry_validity_and_skew() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["expires_at"] = json!(NOW - 1);
    assert_eq!(f.verify_err(&p), UpdateError::Expired);
    let mut p = payload(1, "0.2.0");
    p["expires_at"] = json!(ISSUED + MAX_VALIDITY_SECS + 1);
    assert_eq!(f.verify_err(&p), UpdateError::ValidityTooLong);
    let mut p = payload(1, "0.2.0");
    p["issued_at"] = json!(NOW + 86_401);
    p["expires_at"] = json!(NOW + 86_401 + 1000);
    assert_eq!(f.verify_err(&p), UpdateError::ClockSkew);
    let mut p = payload(1, "0.2.0");
    p["expires_at"] = json!(ISSUED - 1);
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
}

#[test]
fn urls_must_pass_the_host_allowlist() {
    let f = fx();
    for bad in [
        "http://github.com/a/b.exe",
        "https://evil.example/a/b.exe",
        "https://user@github.com/a/b.exe",
        "https://github.com:8443/a/b.exe",
        "https://github.com.evil.example/a",
        "https://g\u{456}thub.com/a",
        "https://github.com\\@evil.example/a",
        "ftp://github.com/a",
        "https://github.com",
        "",
    ] {
        let mut p = payload(1, "0.2.0");
        p["artifacts"][0]["url"] = json!(bad);
        assert_eq!(f.verify_err(&p), UpdateError::BadUrl, "{bad:?}");
    }
}

#[test]
fn url_allowed_table() {
    let ok = |u: &str| url_allowed(u, HOSTS);
    assert!(ok("https://github.com/a"));
    assert!(ok("https://GitHub.com/a"));
    assert!(ok("https://release-assets.githubusercontent.com/x?y=1"));
    for bad in [
        "http://github.com/a",
        "https://github.com",
        "https://github.com?x=/",
        "https://github.com#x",
        "https://github.com./a",
        "https://sub.github.com/a",
        "https://github.com@evil.example/a",
        "https://github.com:443/a",
        "https://[::1]/a",
        "https://github.com%2eevil.example/a",
        "https://github.com/a b",
        "https://github.com/a\tb",
        "https://github.com/\u{e9}",
        "https://\u{0261}ithub.com/a",
        "HTTPS://github.com/a",
        " https://github.com/a",
    ] {
        assert!(!ok(bad), "{bad:?}");
    }
    assert!(!ok(&format!("https://github.com/{}", "a".repeat(2048))));
    assert!(!url_allowed("https://github.com/a", &[]));
}

#[test]
fn counts_and_sizes_are_capped() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    let a = p["artifacts"][0].clone();
    p["artifacts"] = Value::Array(vec![a; 9]);
    assert_eq!(f.verify_err(&p), UpdateError::TooLarge);
    let mut p = payload(1, "0.2.0");
    p["artifacts"][0]["size"] = json!(MAX_ARTIFACT_BYTES + 1);
    assert_eq!(f.verify_err(&p), UpdateError::TooLarge);
    let mut p = payload(1, "0.2.0");
    p["artifacts"][0]["size"] = json!(MAX_ARTIFACT_BYTES);
    assert!(f.verify(&p).is_ok());
    let mut p = payload(1, "0.2.0");
    p["artifacts"][0]["size"] = json!(0);
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
    let mut p = payload(1, "0.2.0");
    p["revoked"] = Value::Array((0..65).map(|i| json!(format!("0.0.{i}"))).collect());
    assert_eq!(f.verify_err(&p), UpdateError::TooLarge);
    let mut p = payload(1, "0.2.0");
    p["notes"] = json!("n".repeat(2001));
    assert_eq!(f.verify_err(&p), UpdateError::TooLarge);
}

#[test]
fn one_artifact_per_platform_and_known_kind() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    let a = p["artifacts"][0].clone();
    p["artifacts"] = json!([a.clone(), a]);
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
    let mut p = payload(1, "0.2.0");
    p["artifacts"][0]["kind"] = json!("msi");
    assert_eq!(f.verify_err(&p), UpdateError::Malformed);
    // A platform this build does not know is ignored; the known one is still found.
    let mut p = payload(1, "0.2.0");
    let a = p["artifacts"][0].clone();
    p["artifacts"] = json!([{
        "platform": "plan9-mips", "kind": "tar", "url": "http://x", "sha256": "bad", "size": 1
    }, a]);
    let m = f.verify(&p).unwrap();
    assert!(m.artifact(Platform::WindowsX86_64).is_some());
}

#[test]
fn sha256_must_be_64_lowercase_hex() {
    let f = fx();
    for bad in [
        SHA.to_uppercase(),
        SHA[..63].to_string(),
        format!("{SHA}0"),
        "g".repeat(64),
    ] {
        let mut p = payload(1, "0.2.0");
        p["artifacts"][0]["sha256"] = json!(bad);
        assert_eq!(f.verify_err(&p), UpdateError::Malformed, "{bad}");
    }
}

#[test]
fn bad_version_strings() {
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-rc1",
        "1.2.3+b",
        "v1.2.3",
        "1..3",
        "1.2.",
        ".1.2",
        "65536.0.0",
        "1.2.99999",
        " 1.2.3",
        "1.2.3 ",
        "1.2.-3",
        "+1.2.3",
        "1.2.\u{663}",
        "١.٢.٣",
    ] {
        assert_eq!(
            Version::parse(bad).unwrap_err(),
            UpdateError::BadVersion,
            "{bad:?}"
        );
    }
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["version"] = json!("0.2");
    assert_eq!(f.verify_err(&p), UpdateError::BadVersion);
    let mut p = payload(1, "0.2.0");
    p["min_supported"] = json!("x");
    assert_eq!(f.verify_err(&p), UpdateError::BadVersion);
    let mut p = payload(1, "0.2.0");
    p["revoked"] = json!(["0.1.01"]);
    assert_eq!(f.verify_err(&p), UpdateError::BadVersion);
}

#[test]
fn version_parse_and_order() {
    let v = |s: &str| Version::parse(s).unwrap();
    assert_eq!(v("0.0.0").to_string(), "0.0.0");
    assert_eq!(v("65535.65535.65535").to_string(), "65535.65535.65535");
    assert!(v("0.2.0") > v("0.1.9"));
    assert!(v("0.10.0") > v("0.9.0"));
    assert!(v("1.0.0") > v("0.65535.65535"));
    assert_eq!(v("1.2.3"), v("1.2.3"));
}

#[test]
fn notes_are_plain_text() {
    let f = fx();
    let mut p = payload(1, "0.2.0");
    p["notes"] = json!("a\u{202e}b\u{200b}c\u{0007}d\te\r\nf\u{e0041}g<b>h</b>\u{feff}i\u{ad}j");
    let m = f.verify(&p).unwrap();
    assert_eq!(m.notes(), "abcd e\nfg<b>h</b>ij");
}

// ---- rollback, replay, revocation ----------------------------------------------------------

#[test]
fn older_version_is_stale_and_lower_serial_is_replayed() {
    let f = fx();
    let m = f.verify(&payload(5, "0.1.5")).unwrap();
    assert_eq!(
        decide(&m, &ctx("0.2.0", 0)).unwrap_err(),
        UpdateError::StaleManifest
    );
    let m = f.verify(&payload(5, "0.3.0")).unwrap();
    assert_eq!(
        decide(&m, &ctx("0.2.0", 6)).unwrap_err(),
        UpdateError::ReplayedManifest
    );
    // Equal serial is fine (same manifest checked twice).
    assert!(decide(&m, &ctx("0.2.0", 5)).is_ok());
}

#[test]
fn same_version_is_up_to_date() {
    let f = fx();
    let m = f.verify(&payload(5, "0.2.0")).unwrap();
    assert_eq!(decide(&m, &ctx("0.2.0", 0)).unwrap().offer, Offer::UpToDate);
}

#[test]
fn revoked_current_offers_a_downgrade_only_and_flags_it() {
    let f = fx();
    let m = f.verify(&payload(5, "0.1.0")).unwrap();
    let adv = decide(&m, &ctx("0.1.1", 0)).unwrap();
    assert!(adv.current_revoked);
    assert_eq!(
        adv.offer,
        Offer::DowngradeOnly {
            version: Version::parse("0.1.0").unwrap()
        }
    );
    // A revoked current version with a newer manifest is a normal update.
    let m = f.verify(&payload(6, "0.2.0")).unwrap();
    let adv = decide(&m, &ctx("0.1.1", 0)).unwrap();
    assert!(adv.current_revoked);
    assert!(matches!(adv.offer, Offer::Available { .. }));
}

#[test]
fn below_min_supported_is_flagged_and_a_revoked_target_is_refused() {
    let f = fx();
    let mut p = payload(5, "0.3.0");
    p["min_supported"] = json!("0.2.0");
    let m = f.verify(&p).unwrap();
    let adv = decide(&m, &ctx("0.1.0", 0)).unwrap();
    assert!(adv.below_min_supported);
    assert!(!decide(&m, &ctx("0.2.0", 0)).unwrap().below_min_supported);
    let mut p = payload(5, "0.3.0");
    p["revoked"] = json!(["0.3.0"]);
    let m = f.verify(&p).unwrap();
    assert_eq!(
        decide(&m, &ctx("0.2.0", 0)).unwrap_err(),
        UpdateError::Revoked
    );
}

#[test]
fn a_manifest_without_this_platform_has_no_artifact() {
    let f = fx();
    let mut p = payload(5, "0.3.0");
    p["artifacts"] = json!([]);
    let m = f.verify(&p).unwrap();
    assert_eq!(
        decide(&m, &ctx("0.2.0", 0)).unwrap_err(),
        UpdateError::NoArtifact
    );
}

// ---- endorsements (rotation) ---------------------------------------------------------------

fn endorsement(
    root: &Ed25519KeyPair,
    root_id: &str,
    newkey: &Ed25519KeyPair,
    id: &str,
    range: (u64, u64),
) -> Value {
    let end = serde_json::to_vec(&json!({
        "keyid": id, "public_key": b64(&public(newkey)), "channel": "stable",
        "not_before_serial": range.0, "not_after_serial": range.1
    }))
    .unwrap();
    envelope_with(root, root_id, ENDORSEMENT_PAYLOAD_TYPE, &end)
}

fn endorsed_manifest(newkey: &Ed25519KeyPair, id: &str, serial: u64, ends: Vec<Value>) -> Vec<u8> {
    let bytes = serde_json::to_vec(&payload(serial, "0.2.0")).unwrap();
    let mut env = envelope_with(newkey, id, MANIFEST_PAYLOAD_TYPE, &bytes);
    env["endorsements"] = Value::Array(ends);
    serde_json::to_vec(&env).unwrap()
}

#[test]
fn endorsed_key_is_accepted_only_inside_its_serial_range() {
    let f = fx();
    let newkey = keypair();
    let e = endorsement(&f.kp, "hb-test-a", &newkey, "hb-new", (10, 20));
    for (serial, ok) in [(9, false), (10, true), (15, true), (20, true), (21, false)] {
        let env = endorsed_manifest(&newkey, "hb-new", serial, vec![e.clone()]);
        let r = verify(&env, f.cfg, NOW);
        if ok {
            assert!(r.is_ok(), "serial {serial}");
        } else {
            assert_eq!(
                r.unwrap_err(),
                UpdateError::NoTrustedSignature,
                "serial {serial}"
            );
        }
    }
}

#[test]
fn endorsement_by_an_unknown_key_is_ignored() {
    let f = fx();
    let stranger = keypair();
    let newkey = keypair();
    let e = endorsement(&stranger, "hb-stranger", &newkey, "hb-new", (0, 100));
    let env = endorsed_manifest(&newkey, "hb-new", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
    // Signed by a trusted keyid but with the wrong key: also ignored (never trusted).
    let e = endorsement(&stranger, "hb-test-a", &newkey, "hb-new", (0, 100));
    let env = endorsed_manifest(&newkey, "hb-new", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
}

#[test]
fn endorsement_chains_deeper_than_one_are_refused() {
    let f = fx();
    let k1 = keypair();
    let k2 = keypair();
    // k1 is endorsed by the embedded key; k2 is endorsed by k1 (depth 2).
    let e1 = endorsement(&f.kp, "hb-test-a", &k1, "hb-k1", (0, 100));
    let mut e2 = endorsement(&k1, "hb-k1", &k2, "hb-k2", (0, 100));
    // A nested endorsement list is not even parseable.
    e2["endorsements"] = json!([e1.clone()]);
    let env = endorsed_manifest(&k2, "hb-k2", 5, vec![e2]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::UnknownField
    );
    // Without nesting, an endorsement signed by k1 (itself not embedded) does not count.
    let e2 = endorsement(&k1, "hb-k1", &k2, "hb-k2", (0, 100));
    let env = endorsed_manifest(&k2, "hb-k2", 5, vec![e1, e2]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
}

#[test]
fn endorsements_cannot_shadow_embedded_ids_or_change_channel_or_exceed_two() {
    let f = fx();
    let newkey = keypair();
    // Same id as the embedded key: ignored.
    let e = endorsement(&f.kp, "hb-test-a", &newkey, "hb-test-a", (0, 100));
    let env = endorsed_manifest(&newkey, "hb-test-a", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
    // Wrong channel.
    let end = serde_json::to_vec(&json!({
        "keyid": "hb-new", "public_key": b64(&public(&newkey)), "channel": "beta",
        "not_before_serial": 0, "not_after_serial": 100
    }))
    .unwrap();
    let e = envelope_with(&f.kp, "hb-test-a", ENDORSEMENT_PAYLOAD_TYPE, &end);
    let env = endorsed_manifest(&newkey, "hb-new", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
    // Inverted range, wrong payload type, three endorsements.
    let e = endorsement(&f.kp, "hb-test-a", &newkey, "hb-new", (50, 10));
    let env = endorsed_manifest(&newkey, "hb-new", 20, vec![e.clone()]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
    let env = endorsed_manifest(&newkey, "hb-new", 20, vec![e.clone(), e.clone(), e]);
    assert_eq!(verify(&env, f.cfg, NOW).unwrap_err(), UpdateError::TooLarge);
    let end = serde_json::to_vec(&json!({
        "keyid": "hb-new", "public_key": b64(&public(&newkey)), "channel": "stable",
        "not_before_serial": 0, "not_after_serial": 100
    }))
    .unwrap();
    let e = envelope_with(&f.kp, "hb-test-a", MANIFEST_PAYLOAD_TYPE, &end);
    let env = endorsed_manifest(&newkey, "hb-new", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::NoTrustedSignature
    );
}

#[test]
fn a_bad_signature_by_an_endorsed_key_is_bad_signature() {
    let f = fx();
    let newkey = keypair();
    let other = keypair();
    let e = endorsement(&f.kp, "hb-test-a", &newkey, "hb-new", (0, 100));
    let env = endorsed_manifest(&other, "hb-new", 5, vec![e]);
    assert_eq!(
        verify(&env, f.cfg, NOW).unwrap_err(),
        UpdateError::BadSignature
    );
}

// ---- configuration -------------------------------------------------------------------------

#[test]
fn the_real_build_is_unconfigured_until_keys_are_added() {
    assert!(
        TRUSTED_KEYS.is_empty(),
        "TODO(S3-7): no real key is committed yet"
    );
    assert!(matches!(embedded(), UpdateConfig::Unconfigured));
}

#[test]
fn verify_with_no_keys_is_not_configured() {
    let cfg = config(&[]);
    let kp = keypair();
    assert_eq!(
        verify(&signed(&kp, "x", &payload(1, "0.2.0")), cfg, NOW).unwrap_err(),
        UpdateError::NotConfigured
    );
}

#[test]
fn error_codes_are_the_display_text_and_unique() {
    let all = [
        UpdateError::NotConfigured,
        UpdateError::TooLarge,
        UpdateError::Malformed,
        UpdateError::UnknownField,
        UpdateError::NoTrustedSignature,
        UpdateError::BadSignature,
        UpdateError::WrongProduct,
        UpdateError::WrongChannel,
        UpdateError::ClockSkew,
        UpdateError::Expired,
        UpdateError::ValidityTooLong,
        UpdateError::StaleManifest,
        UpdateError::ReplayedManifest,
        UpdateError::BadUrl,
        UpdateError::NoArtifact,
        UpdateError::BadVersion,
        UpdateError::Revoked,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for e in all {
        assert_eq!(e.to_string(), e.code());
        assert!(e
            .code()
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_'));
        assert!(seen.insert(e.code()), "duplicate {}", e.code());
    }
    assert_eq!(UpdateError::NotConfigured.code(), "updates_not_configured");
    assert_eq!(UpdateError::Expired.code(), "manifest_expired");
}

#[test]
fn platform_and_channel_names() {
    assert_eq!(Channel::Stable.as_str(), "stable");
    assert_eq!(Platform::WindowsX86_64.as_str(), "windows-x86_64");
    assert_eq!(
        Platform::current().is_some(),
        cfg!(all(windows, target_arch = "x86_64"))
    );
}

// ---- no-panic over hostile bytes -----------------------------------------------------------

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn mutated_and_random_bytes_never_panic_and_never_verify() {
    let f = fx();
    let newkey = keypair();
    let e = endorsement(&f.kp, "hb-test-a", &newkey, "hb-new", (0, 100));
    let seeds = [
        f.env(&payload(1, "0.2.0")),
        endorsed_manifest(&newkey, "hb-new", 5, vec![e]),
    ];
    let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15);
    for seed in &seeds {
        assert!(verify(seed, f.cfg, NOW).is_ok());
        for _ in 0..3000 {
            let mut b = seed.clone();
            for _ in 0..1 + rng.below(4) {
                if b.is_empty() {
                    break;
                }
                let i = rng.below(b.len());
                match rng.below(5) {
                    0 => b[i] ^= 1 << rng.below(8),
                    1 => b[i] = rng.next() as u8,
                    2 => {
                        b.remove(i);
                    }
                    3 => b.insert(i, rng.next() as u8),
                    _ => b.truncate(i),
                }
            }
            if b != *seed {
                assert!(verify(&b, f.cfg, NOW).is_err());
            }
        }
    }
    for _ in 0..2000 {
        let n = rng.below(300);
        let b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        assert!(verify(&b, f.cfg, NOW).is_err());
    }
    // Hostile but signed payloads (the signature is valid, so the payload parser sees them).
    for junk in [
        &b"{}"[..],
        b"null",
        b"\xff\xff",
        b"[1,2]",
        b"{\"schema\":1}",
        &[b'{'; 5000],
    ] {
        let env = envelope_with(&f.kp, "hb-test-a", MANIFEST_PAYLOAD_TYPE, junk);
        assert!(verify(&serde_json::to_vec(&env).unwrap(), f.cfg, NOW).is_err());
    }
}

// ---- launch: the decisions that can be tested anywhere -------------------------------------

#[test]
fn launch_command_line_is_the_quoted_path_only() {
    assert_eq!(
        command_line(r"C:\Users\a b\AppData\Local\x\run\00\setup.exe").unwrap(),
        "\"C:\\Users\\a b\\AppData\\Local\\x\\run\\00\\setup.exe\""
    );
    for bad in ["", "a\"b", "a\0b"] {
        assert_eq!(command_line(bad).unwrap_err(), LaunchError::UnsafePath);
    }
}

#[test]
fn launch_names_and_paths_are_fixed() {
    assert_eq!(SETUP_FILE_NAME, "setup.exe");
    let dir = std::path::Path::new("run").join(run_dir_name(&[0xab; 16]));
    assert_eq!(setup_path(&dir).file_name().unwrap(), "setup.exe");
    assert_eq!(run_dir_name(&[0; 16]), "0".repeat(32));
    assert_eq!(run_dir_name(&[0xab; 16]), "ab".repeat(16));
    assert!(is_run_dir_name(&"ab".repeat(16)));
    for bad in [
        "",
        "AB",
        &"ab".repeat(15),
        &"ab".repeat(17),
        &"zz".repeat(16),
        "..",
        "kept",
        &"AB".repeat(16),
    ] {
        assert!(!is_run_dir_name(bad), "{bad:?}");
    }
}

#[test]
fn launch_directory_acl_is_protected_and_not_everyone() {
    assert!(
        PRIVATE_DIR_SDDL.starts_with("D:P("),
        "protected DACL: no inheritance from the parent"
    );
    for broad in ["WD", "BU", "AU", "IU", "BA"] {
        assert!(
            !PRIVATE_DIR_SDDL.contains(&format!(";;;{broad})")),
            "{broad} must not be in the ACL"
        );
    }
    assert!(PRIVATE_DIR_SDDL.contains(";;;SY)") && PRIVATE_DIR_SDDL.contains(";;;OW)"));
}

#[test]
fn launch_error_codes() {
    let all = [
        (LaunchError::Io, "io_error"),
        (LaunchError::HashMismatch, "hash_mismatch"),
        (LaunchError::UnsafePath, "unsafe_path"),
        (LaunchError::NeedsAdmin, "installer_needs_admin"),
        (LaunchError::Blocked, "launch_blocked"),
        (LaunchError::Unsupported, "unsupported_platform"),
    ];
    for (e, c) in all {
        assert_eq!(e.code(), c);
        assert_eq!(e.to_string(), c);
    }
}
