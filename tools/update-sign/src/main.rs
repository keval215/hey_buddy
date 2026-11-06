//! `update-sign keygen|sign|verify` (ADR 0005 section 6, S3-3). OFFLINE maintainer tool.
//!
//! - Keys are read and written ONLY at paths given on the command line; nothing is ever read
//!   from the environment or a default location, and no key is printed.
//! - `keygen` refuses to write a private key anywhere under a git work tree (any ancestor
//!   directory holding `.git`), and never overwrites a file.
//! - CI only builds and tests this tool with ephemeral keys; the real keys never touch CI or a
//!   networked build machine.
//!
//! ```text
//! update-sign keygen --id hb-2026-a --private <file> --public <file>
//! update-sign sign   --private <file> --id hb-2026-a --payload <payload.json> --out <envelope.json>
//! update-sign verify --public <file> --id hb-2026-a --envelope <envelope.json> [--now <unix>] [--host <h>]...
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use update::{Channel, TrustConfig, TrustedKey, MANIFEST_PAYLOAD_TYPE};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout();
    if let Err(e) = run(&args, &mut out) {
        eprintln!("update-sign: {e}");
        std::process::exit(1);
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    let all = flags(args, name);
    match all.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("missing {name}")),
        _ => Err(format!("{name} given more than once")),
    }
}

fn flags<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].as_str())
        .collect()
}

/// A directory that holds `.git` is a work tree.
fn inside_git_work_tree(path: &Path) -> bool {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |c| c.join(path))
    };
    // Resolve the deepest existing ancestor (symlinks included), then re-append the rest.
    let mut existing = abs.clone();
    let mut rest = PathBuf::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(n), Some(p)) => {
                rest = Path::new(n).join(&rest);
                existing = p.to_path_buf();
            }
            _ => break,
        }
    }
    let resolved = existing.canonicalize().unwrap_or(existing).join(rest);
    resolved
        .ancestors()
        .skip(1)
        .any(|a| a.join(".git").exists())
}

fn write_new(path: &Path, data: &[u8], secret: bool) -> Result<(), String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let _ = secret;
    let mut f = o
        .open(path)
        .map_err(|e| format!("cannot create {}: {}", path.display(), e.kind()))?;
    f.write_all(data).map_err(|_| "write failed".to_string())
}

fn read_key(path: &str) -> Result<Ed25519KeyPair, String> {
    let text = std::fs::read_to_string(path).map_err(|_| "cannot read the private key file")?;
    let der = STANDARD
        .decode(text.trim())
        .map_err(|_| "the private key file is not base64")?;
    Ed25519KeyPair::from_pkcs8(&der).map_err(|_| "not a valid ed25519 private key".to_string())
}

fn read_public(path: &str) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|_| "cannot read the public key file")?;
    STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| "the public key file must hold 32 bytes of base64".to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_graphic())
}

pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let Some(cmd) = args.first() else {
        return Err("usage: update-sign keygen|sign|verify ...".into());
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "keygen" => {
            let id = flag(rest, "--id")?;
            let (private, public) = (flag(rest, "--private")?, flag(rest, "--public")?);
            if !valid_id(id) {
                return Err("invalid --id".into());
            }
            for p in [private, public] {
                if inside_git_work_tree(Path::new(p)) {
                    return Err(format!(
                        "refusing to write key material into a git work tree: {p}"
                    ));
                }
            }
            let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| "key generation failed")?;
            let kp =
                Ed25519KeyPair::from_pkcs8(doc.as_ref()).map_err(|_| "key generation failed")?;
            let pub_b64 = STANDARD.encode(kp.public_key().as_ref());
            write_new(
                Path::new(private),
                format!("{}\n", STANDARD.encode(doc.as_ref())).as_bytes(),
                true,
            )?;
            write_new(Path::new(public), format!("{pub_b64}\n").as_bytes(), false)?;
            let bytes: Vec<String> = kp
                .public_key()
                .as_ref()
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect();
            writeln!(out, "key id: {id}\npublic key (base64): {pub_b64}")
                .map_err(|_| "write failed")?;
            writeln!(
                out,
                "TrustedKey {{ id: \"{id}\", public: [{}] }}",
                bytes.join(", ")
            )
            .map_err(|_| "write failed".to_string())
        }
        "sign" => {
            let kp = read_key(flag(rest, "--private")?)?;
            let id = flag(rest, "--id")?;
            if !valid_id(id) {
                return Err("invalid --id".into());
            }
            let payload = std::fs::read(flag(rest, "--payload")?)
                .map_err(|_| "cannot read the payload file")?;
            // Refuse to sign something that is not even JSON (the app parses it after verifying).
            serde_json::from_slice::<serde_json::Value>(&payload)
                .map_err(|_| "the payload is not valid JSON")?;
            let sig = kp.sign(&update::dsse::pae(MANIFEST_PAYLOAD_TYPE, &payload));
            let env = serde_json::json!({
                "payloadType": MANIFEST_PAYLOAD_TYPE,
                "payload": STANDARD.encode(&payload),
                "signatures": [{"keyid": id, "sig": STANDARD.encode(sig.as_ref())}],
            });
            let bytes = serde_json::to_vec(&env).map_err(|_| "encode failed")?;
            write_new(Path::new(flag(rest, "--out")?), &bytes, false)?;
            writeln!(out, "signed {} payload bytes", payload.len())
                .map_err(|_| "write failed".to_string())
        }
        "verify" => {
            let public = read_public(flag(rest, "--public")?)?;
            let id: &'static str = Box::leak(flag(rest, "--id")?.to_string().into_boxed_str());
            let envelope = std::fs::read(flag(rest, "--envelope")?)
                .map_err(|_| "cannot read the envelope file")?;
            let now = match flags(rest, "--now").as_slice() {
                [] => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                [n] => n.parse::<u64>().map_err(|_| "--now must be unix seconds")?,
                _ => return Err("--now given more than once".into()),
            };
            let mut hosts: Vec<&'static str> = vec![
                "github.com",
                "release-assets.githubusercontent.com",
                "objects.githubusercontent.com",
            ];
            for h in flags(rest, "--host") {
                hosts.push(Box::leak(h.to_string().into_boxed_str()));
            }
            let keys: &'static [TrustedKey] = Box::leak(Box::new([TrustedKey { id, public }]));
            let cfg = TrustConfig {
                manifest_url: "",
                hosts: Box::leak(hosts.into_boxed_slice()),
                product: "dev.heybuddy.desktop",
                channel: Channel::Stable,
                keys,
            };
            let m = update::verify(&envelope, &cfg, now).map_err(|e| e.code().to_string())?;
            writeln!(out, "OK version {} serial {}", m.version(), m.serial())
                .map_err(|_| "write failed".to_string())
        }
        _ => Err("usage: update-sign keygen|sign|verify ...".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn payload() -> String {
        serde_json::json!({
            "schema": 1, "product": "dev.heybuddy.desktop", "channel": "stable",
            "serial": 3, "version": "0.2.0", "issued_at": 1_793_750_400u64,
            "expires_at": 1_793_750_400u64 + 86_400 * 90, "min_supported": "0.1.0",
            "revoked": [], "notes": "n",
            "artifacts": [{"platform": "windows-x86_64", "kind": "nsis",
                "url": "https://github.com/o/hey_buddy/releases/download/v0.2.0/x.exe",
                "sha256": "0".repeat(64), "size": 10}]
        })
        .to_string()
    }

    fn p(d: &Path, n: &str) -> String {
        d.join(n).to_str().unwrap().to_string()
    }

    #[test]
    fn keygen_sign_verify_round_trip_and_tampering_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut out = Vec::new();
        run(
            &s(&[
                "keygen",
                "--id",
                "hb-t",
                "--private",
                &p(d, "k"),
                "--public",
                &p(d, "k.pub"),
            ]),
            &mut out,
        )
        .unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("TrustedKey") && shown.contains("hb-t"));
        let private = std::fs::read_to_string(d.join("k")).unwrap();
        assert!(
            !shown.contains(private.trim()),
            "the private key is never printed"
        );
        std::fs::write(d.join("payload.json"), payload()).unwrap();
        run(
            &s(&[
                "sign",
                "--private",
                &p(d, "k"),
                "--id",
                "hb-t",
                "--payload",
                &p(d, "payload.json"),
                "--out",
                &p(d, "env.json"),
            ]),
            &mut Vec::new(),
        )
        .unwrap();
        let mut out = Vec::new();
        let ok = [
            "verify",
            "--public",
            &p(d, "k.pub"),
            "--id",
            "hb-t",
            "--envelope",
            &p(d, "env.json"),
            "--now",
            "1793800000",
        ];
        run(&s(&ok), &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap().trim(),
            "OK version 0.2.0 serial 3"
        );
        // Expired.
        let mut late = ok.to_vec();
        late[8] = "9999999999";
        assert_eq!(
            run(&s(&late), &mut Vec::new()).unwrap_err(),
            "manifest_expired"
        );
        // Tampered envelope.
        let mut bytes = std::fs::read(d.join("env.json")).unwrap();
        let i = bytes.len() / 2;
        bytes[i] ^= 1;
        std::fs::write(d.join("env2.json"), bytes).unwrap();
        let mut bad = ok.to_vec();
        let env2 = p(d, "env2.json");
        bad[6] = &env2;
        assert!(run(&s(&bad), &mut Vec::new()).is_err());
        // Another key does not verify it.
        run(
            &s(&[
                "keygen",
                "--id",
                "hb-o",
                "--private",
                &p(d, "o"),
                "--public",
                &p(d, "o.pub"),
            ]),
            &mut Vec::new(),
        )
        .unwrap();
        let other_pub = p(d, "o.pub");
        let mut wrong = ok.to_vec();
        wrong[2] = &other_pub;
        assert_eq!(
            run(&s(&wrong), &mut Vec::new()).unwrap_err(),
            "bad_signature"
        );
    }

    #[test]
    fn keygen_refuses_a_git_work_tree_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("keys")).unwrap();
        for target in [
            repo.join("k"),
            repo.join("keys").join("k"),
            repo.join("new").join("k"),
        ] {
            let t = target.to_str().unwrap();
            let e = run(
                &s(&[
                    "keygen",
                    "--id",
                    "x",
                    "--private",
                    t,
                    "--public",
                    &p(dir.path(), "pub"),
                ]),
                &mut Vec::new(),
            )
            .unwrap_err();
            assert!(e.contains("git work tree"), "{e}");
            assert!(!target.exists());
        }
        // Outside any repo it works once, and refuses to overwrite.
        let args = s(&[
            "keygen",
            "--id",
            "x",
            "--private",
            &p(dir.path(), "k"),
            "--public",
            &p(dir.path(), "k.pub"),
        ]);
        run(&args, &mut Vec::new()).unwrap();
        assert!(run(&args, &mut Vec::new()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("k"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "private key file is owner-only");
        }
    }

    #[test]
    fn bad_arguments_are_errors_not_panics() {
        for args in [
            s(&[]),
            s(&["nope"]),
            s(&["keygen"]),
            s(&[
                "sign",
                "--private",
                "/nonexistent",
                "--id",
                "x",
                "--payload",
                "/nonexistent",
                "--out",
                "/tmp/never",
            ]),
            s(&[
                "verify",
                "--public",
                "/nonexistent",
                "--id",
                "x",
                "--envelope",
                "/nonexistent",
            ]),
            s(&[
                "keygen",
                "--id",
                "a b",
                "--private",
                "/tmp/zz-never-1",
                "--public",
                "/tmp/zz-never-2",
            ]),
        ] {
            assert!(run(&args, &mut Vec::new()).is_err(), "{args:?}");
        }
    }

    #[test]
    fn sign_refuses_a_payload_that_is_not_json() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        run(
            &s(&[
                "keygen",
                "--id",
                "x",
                "--private",
                &p(d, "k"),
                "--public",
                &p(d, "k.pub"),
            ]),
            &mut Vec::new(),
        )
        .unwrap();
        std::fs::write(d.join("bad"), b"not json").unwrap();
        let e = run(
            &s(&[
                "sign",
                "--private",
                &p(d, "k"),
                "--id",
                "x",
                "--payload",
                &p(d, "bad"),
                "--out",
                &p(d, "o"),
            ]),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(e.contains("JSON"));
        assert!(!d.join("o").exists());
    }
}
