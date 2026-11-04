//! `installer-check`: offline assertions for the Windows installer (S3-1).
//!
//! Static mode (default, runs on Linux CI): checks tauri.conf.json, the installer overlay, the NSIS
//! hooks and the source manifest. Artifact flags add checks of built files:
//!   --installer <setup.exe>   exists, is a PE file; its own manifest (if readable) is asInvoker.
//!                             If the manifest cannot be extracted (NSIS may compress it) a NOTE is
//!                             printed; add --require-installer-manifest to make that a failure.
//!   --require-installer-manifest  fail when the installer's manifest cannot be extracted
//!   --exe <app.exe>           the Tauri-produced main app exe (likely desktop.exe; verify on
//!                             Windows): embedded manifest PerMonitorV2, asInvoker, no uiAccess
//!   --agent <buddy-agent.exe> exists (it ships beside the main app exe in the install dir)
//! Unsigned installers are expected in S3-1: that is noted, not failed.

use std::{env, fs, path::PathBuf, process::exit};

use installer_check::{check_config, check_manifest, extract_manifest};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tauri = root.join("apps/desktop/src-tauri");
    let mut bad: Vec<String> = Vec::new();

    let read = |p: PathBuf| fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()));
    let json = |p: PathBuf| {
        read(p)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()))
    };
    match (
        json(tauri.join("tauri.conf.json")),
        json(tauri.join("tauri.installer.conf.json")),
    ) {
        (Ok(base), Ok(overlay)) => {
            let hooks = read(tauri.join("installer/hooks.nsh")).ok();
            bad.extend(check_config(&base, &overlay, hooks.as_deref()));
        }
        (a, b) => bad.push(format!("config unreadable: {:?} {:?}", a.err(), b.err())),
    }
    match read(tauri.join("windows-app-manifest.xml")) {
        Ok(x) => bad.extend(check_manifest(&x)),
        Err(e) => bad.push(format!("source manifest unreadable: {e}")),
    }

    let mut args: Vec<String> = env::args().skip(1).collect();
    let require_manifest = args.iter().any(|a| a == "--require-installer-manifest");
    args.retain(|a| a != "--require-installer-manifest");
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let path = args.get(i + 1).map(PathBuf::from);
        let Some(path) = path else {
            bad.push(format!("{flag} needs a path"));
            break;
        };
        match (flag, fs::read(&path)) {
            (_, Err(e)) => bad.push(format!("{flag} {}: {e}", path.display())),
            ("--installer", Ok(b)) => {
                if !b.starts_with(b"MZ") {
                    bad.push("installer is not a PE file".into());
                }
                // The NSIS stub's own manifest, when stored uncompressed, must not elevate.
                match extract_manifest(&b) {
                    Some(m) => bad.extend(check_manifest_level_only(&m)),
                    None if require_manifest => bad.push(
                        "installer: embedded manifest could not be extracted \
                         (--require-installer-manifest)"
                            .into(),
                    ),
                    None => println!(
                        "NOTE: the installer's embedded manifest could not be extracted \
                         (likely compressed); its elevation level was NOT checked."
                    ),
                }
            }
            ("--exe", Ok(b)) => match extract_manifest(&b) {
                Some(m) => bad.extend(check_manifest(&m)),
                None => bad.push("exe: no embedded manifest found".into()),
            },
            ("--agent", Ok(b)) => {
                if !b.starts_with(b"MZ") {
                    bad.push("agent is not a PE file".into());
                }
            }
            _ => bad.push(format!("unknown flag {flag}")),
        }
        i += 2;
    }

    println!("note: the installer is UNSIGNED (S3-1); SmartScreen will warn.");
    if bad.is_empty() {
        println!("installer-check: OK");
    } else {
        for b in &bad {
            eprintln!("FAIL {b}");
        }
        exit(1);
    }
}

/// The installer stub's manifest need not declare DPI awareness; only elevation is checked.
fn check_manifest_level_only(m: &str) -> Vec<String> {
    check_manifest(m)
        .into_iter()
        .filter(|f| !f.contains("PerMonitorV2"))
        .collect()
}
