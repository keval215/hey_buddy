//! Static checks on `models/registry.json` (S3-5). Pure and offline.
//!
//! On top of the loader's own validation (`voice::registry::Registry::from_json_str`: schema,
//! unique ids, profile references, safe file paths, sha256 shape) this enforces:
//! - at least [`MIN_ENTRIES`] entries (so the check can never pass on an empty file),
//! - a `default` profile,
//! - a download URL is either exactly `TODO` or an `https://` URL without credentials,
//! - a URL and its sha256 are filled in together: no real URL with a `TODO` checksum and no
//!   checksum without a URL (also per file of directory models).
//!
//! Filling in the values is a maintainer step (see `docs/models.md`); this check never invents
//! or fetches anything.

use voice::registry::Registry;

/// A registry with fewer entries than this is a broken file, not a small one.
pub const MIN_ENTRIES: usize = 10;

/// What the check looked at (printed by the binary so CI can assert it is not vacuous).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    pub profiles: usize,
    /// Entries (or files) with a real URL and checksum.
    pub with_source: usize,
}

fn is_todo(s: &str) -> bool {
    s == "TODO"
}

/// `https://host...` with a non-empty host and no `user[:pass]@`.
pub fn is_https_url(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && !authority.contains(char::is_whitespace)
        && !u.contains(char::is_whitespace)
}

fn pair(what: &str, url: &str, sha: &str, errs: &mut Vec<String>) -> bool {
    match (is_todo(url), is_todo(sha)) {
        (true, true) => false,
        (false, false) => {
            if !is_https_url(url) {
                errs.push(format!("{what}: url must be https without credentials"));
            }
            true
        }
        (false, true) => {
            errs.push(format!("{what}: url is set but sha256 is TODO"));
            false
        }
        (true, false) => {
            errs.push(format!("{what}: sha256 is set but url is TODO"));
            false
        }
    }
}

/// Check the registry JSON text. `Err` lists every problem found (fixed wording, entry ids only).
pub fn check(json: &str) -> Result<Summary, Vec<String>> {
    let reg = Registry::from_json_str(json).map_err(|e| vec![format!("registry invalid: {e}")])?;
    let mut errs = Vec::new();
    if reg.models.len() < MIN_ENTRIES {
        errs.push(format!(
            "registry has {} entries, expected at least {MIN_ENTRIES}",
            reg.models.len()
        ));
    }
    if !reg.profiles.contains_key("default") {
        errs.push("no `default` profile".to_string());
    }
    let mut with_source = 0;
    for m in &reg.models {
        if m.files.is_none() && pair(&m.id, &m.url, &m.sha256, &mut errs) {
            with_source += 1;
        }
        if let Some(files) = &m.files {
            for f in files {
                if pair(
                    &format!("{}/{}", m.id, f.path),
                    &f.url,
                    &f.sha256,
                    &mut errs,
                ) {
                    with_source += 1;
                }
            }
        }
    }
    if errs.is_empty() {
        Ok(Summary {
            entries: reg.models.len(),
            profiles: reg.profiles.len(),
            with_source,
        })
    } else {
        Err(errs)
    }
}

#[cfg(test)]
mod tests;
