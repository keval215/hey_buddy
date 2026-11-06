//! Maintainer tool (S3-5): fetch one URL the maintainer supplies with the app's own
//! downloader, then print its size and sha256 for review.
//!
//! - Explicit run only: nothing in the app or CI calls this.
//! - HTTPS only, no credentials in the URL, redirects re-checked (all in the app's
//!   `Downloader`; there is no second copy of that logic here).
//! - The download goes to a fresh temp directory that is removed afterwards.
//! - `models/registry.json` is read (to check the id and show the licence) and never written.
//! - Output is values, not edits: the maintainer reads the licence file, then pastes by hand.

use std::io::Write;
use std::path::PathBuf;

use buddy_core::download::{fetch_report, Downloader, DEFAULT_MAX_BYTES};
use voice::registry::Registry;

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub id: String,
    pub url: String,
    pub max_bytes: u64,
    pub registry: PathBuf,
}

pub const USAGE: &str =
    "usage: registry-verify --id <entry id> --url <https url> [--max-bytes N] [--registry <file>]";

/// Parse the arguments. Errors are fixed strings.
pub fn parse_args(args: &[String]) -> Result<Args, &'static str> {
    let (mut id, mut url, mut registry) = (None, None, None);
    let mut max_bytes = DEFAULT_MAX_BYTES;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or("missing value");
        match a.as_str() {
            "--id" => id = Some(val()?),
            "--url" => url = Some(val()?),
            "--registry" => registry = Some(PathBuf::from(val()?)),
            "--max-bytes" => {
                max_bytes = val()?
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("bad --max-bytes")?
            }
            _ => return Err("unknown argument"),
        }
    }
    Ok(Args {
        id: id.ok_or("missing --id")?,
        url: url.ok_or("missing --url")?,
        max_bytes,
        registry: registry.unwrap_or_else(|| PathBuf::from("models/registry.json")),
    })
}

/// The URL without query, fragment and final path segment, as a place to look for a licence.
/// `None` when the URL has no path to strip.
pub fn licence_hint_base(url: &str) -> Option<String> {
    let cut = url.split(['?', '#']).next()?;
    let (head, _file) = cut.rsplit_once('/')?;
    // `https://host` alone has no directory.
    head.contains("://")
        .then_some(head)
        .filter(|h| h.rsplit_once("://").is_some_and(|(_, r)| r.contains('/')))
        .map(str::to_string)
}

/// Do the work. `Err` is a fixed token or short fixed message.
pub fn run(args: &Args, downloader: &Downloader, out: &mut dyn Write) -> Result<(), String> {
    let reg = Registry::load(&args.registry).map_err(|_| "cannot load the registry".to_string())?;
    let entry = reg
        .get(&args.id)
        .map_err(|_| "unknown_model: no such id in the registry".to_string())?;
    // `fetch_report` validates the URL (https, no userinfo) before any request is made.
    let tmp = tempfile::Builder::new()
        .prefix("registry-verify-")
        .tempdir()
        .map_err(|_| "io_error: cannot create a temp directory".to_string())?;
    let progress = |bytes: u64, _total: Option<u64>| {
        let _ = writeln!(std::io::stderr(), "downloaded {bytes} bytes");
    };
    let report = fetch_report(downloader, &args.url, tmp.path(), args.max_bytes, &progress)
        .map_err(|e| e.code().to_string());
    // Explicit cleanup so a failure to remove is visible; the guard would do it on drop too.
    let cleaned = tmp.close().is_ok();
    let report = report?;

    let w = |e: std::io::Error| e.to_string();
    writeln!(
        out,
        "registry-verify: REVIEW these values, then paste them by hand. Nothing was written."
    )
    .map_err(w)?;
    writeln!(out, "id: {}", entry.id).map_err(w)?;
    writeln!(out, "size_bytes: {}", report.size).map_err(w)?;
    writeln!(out, "sha256: {}", report.sha256).map_err(w)?;
    writeln!(out, "licence (registry says): {}", entry.licence).map_err(w)?;
    match licence_hint_base(&args.url) {
        Some(b) => writeln!(
            out,
            "licence file hint: read LICENSE / LICENSE.txt / README next to {b}/ and confirm the licence matches"
        ),
        None => writeln!(
            out,
            "licence file hint: find the model's LICENSE / README at its home page and confirm the licence"
        ),
    }
    .map_err(w)?;
    if entry.files.is_some() {
        writeln!(
            out,
            "directory model: run once per file and paste into the matching `files[]` item (entry-level url/sha256 stay TODO)"
        )
        .map_err(w)?;
    }
    if !cleaned {
        writeln!(out, "warning: the temp directory could not be removed").map_err(w)?;
    }
    Ok(())
}

/// Entry point used by `main` and the tests; returns the exit code.
pub fn main_with(
    args: &[String],
    downloader: &Downloader,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(m) => {
            let _ = writeln!(err, "registry-verify: {m}\n{USAGE}");
            return 2;
        }
    };
    match run(&parsed, downloader, out) {
        Ok(()) => 0,
        Err(m) => {
            let _ = writeln!(err, "registry-verify: {m}");
            1
        }
    }
}
