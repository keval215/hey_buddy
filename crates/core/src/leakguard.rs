//! Debug tripwire for title / URL leaks (S2-5, spec item 4).
//!
//! The tree reader never reads window, tab or page titles, and `redact_tree` scrubs URLs, so no
//! production code HAS a title to compare against. This module is therefore fed ONLY by tests,
//! fakes or an explicit debug hook: [`register_marker`] adds a string to a process-local
//! [`TitleLedger`], and [`assert_no_known_titles`] panics if a built `PreparedRequest` (body
//! bytes, system prompt, transcript, screen text, endpoint, model) contains any of them.
//!
//! This is a tripwire for regressions, NOT a production control: it exists only under
//! `cfg(debug_assertions)`; in release builds [`register_marker`] and
//! [`assert_no_known_titles`] are empty functions and nothing is ever stored or scanned. The
//! panic message names the marker's index, never its content (no content in errors or logs).
//!
//! Matching is normalised (lower-cased, alphanumerics only, so quoting, spacing and JSON
//! escaping do not hide a leak) and also detects fragments: any [`SHINGLE`]-character window of a
//! registered string (long enough, about three words, that ordinary words in the system prompt
//! such as "instructions" cannot trigger it). Markers that normalise to fewer than [`MIN_MARKER`] characters are ignored
//! (too noisy to be meaningful).

use llm::PreparedRequest;

/// Registered strings shorter than this (normalised) are ignored.
pub const MIN_MARKER: usize = 8;
/// Fragment length (normalised characters) used for partial-leak detection.
pub const SHINGLE: usize = 20;

/// Lower-case, keep alphanumerics only.
fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Process-local set of strings that must never appear in a request.
#[derive(Debug, Default)]
pub struct TitleLedger {
    markers: Vec<String>,
}

impl TitleLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `false` if the marker is too short after normalisation (not stored).
    pub fn register(&mut self, marker: &str) -> bool {
        let n = normalise(marker);
        if n.chars().count() < MIN_MARKER {
            return false;
        }
        if !self.markers.contains(&n) {
            self.markers.push(n);
        }
        true
    }

    pub fn clear(&mut self) {
        self.markers.clear();
    }

    pub fn len(&self) -> usize {
        self.markers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    /// Index of the first registered marker found in `haystack` (a whole marker, or any
    /// [`SHINGLE`]-window of one longer than that).
    pub fn find_in(&self, haystack: &str) -> Option<usize> {
        let h = normalise(haystack);
        if h.is_empty() {
            return None;
        }
        for (i, m) in self.markers.iter().enumerate() {
            if h.contains(m.as_str()) {
                return Some(i);
            }
            let chars: Vec<char> = m.chars().collect();
            if chars.len() > SHINGLE {
                let mut w = String::with_capacity(SHINGLE * 4);
                for win in chars.windows(SHINGLE) {
                    w.clear();
                    w.extend(win);
                    if h.contains(w.as_str()) {
                        return Some(i);
                    }
                }
            }
        }
        None
    }
}

#[cfg(debug_assertions)]
fn global() -> &'static std::sync::Mutex<TitleLedger> {
    static LEDGER: std::sync::OnceLock<std::sync::Mutex<TitleLedger>> = std::sync::OnceLock::new();
    LEDGER.get_or_init(Default::default)
}

/// Test / fake / explicit-debug hook: forbid `marker` (and its fragments) in every request
/// built from now on. Debug builds only; a no-op in release.
pub fn register_marker(marker: &str) {
    #[cfg(debug_assertions)]
    {
        global()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .register(marker);
    }
    #[cfg(not(debug_assertions))]
    let _ = marker;
}

/// Forget every registered marker (tests).
pub fn clear_markers() {
    #[cfg(debug_assertions)]
    global().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg_attr(not(any(debug_assertions, test)), allow(dead_code))]
/// Strings of a JSON value (keys included). Long blob-like strings (images) are skipped.
fn json_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => {
            if !(s.len() > 32 * 1024 && !s.contains(char::is_whitespace)) {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| json_strings(x, out)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                out.push(k.clone());
                json_strings(x, out);
            }
        }
        _ => {}
    }
}

/// Everything a request carries as text: body strings (JSON-decoded, so escapes cannot hide a
/// marker), the raw body when it is not JSON, and the preview strings.
#[cfg(debug_assertions)]
fn request_texts(req: &PreparedRequest) -> Vec<String> {
    let mut texts = Vec::new();
    match serde_json::from_slice::<serde_json::Value>(req.body()) {
        Ok(v) => json_strings(&v, &mut texts),
        Err(_) => texts.push(String::from_utf8_lossy(req.body()).into_owned()),
    }
    let p = req.preview();
    texts.push(p.system.to_string());
    texts.push(p.transcript.to_string());
    if let Some(t) = &p.screen_text {
        texts.push(t.to_string());
    }
    texts.push(p.endpoint.to_string());
    texts.push(p.model.to_string());
    texts
}

/// Panics (debug builds only) if the request contains a registered marker. Wired into
/// `Buddy::prepare_and_consent`, so every request a turn builds passes through it.
pub fn assert_no_known_titles(req: &PreparedRequest) {
    #[cfg(debug_assertions)]
    {
        let ledger = global().lock().unwrap_or_else(|e| e.into_inner());
        if ledger.is_empty() {
            return;
        }
        for t in request_texts(req) {
            if let Some(i) = ledger.find_in(&t) {
                // Never print the marker or the text.
                panic!("leakguard: a registered title/URL marker (#{i}) is in a built request");
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = req;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_ignores_case_spacing_and_punctuation() {
        assert_eq!(normalise("Evil.Example/Reset?x=1"), "evilexampleresetx1");
        assert_eq!(normalise("  \u{2013}Ünï "), "ünï");
    }

    #[test]
    fn short_markers_are_ignored() {
        let mut l = TitleLedger::new();
        assert!(!l.register("abc"));
        assert!(!l.register("a-b-c-d!"));
        assert!(l.is_empty());
        assert!(l.register("12345678"));
        assert_eq!(l.len(), 1);
        l.register("1234-5678"); // duplicate after normalisation
        assert_eq!(l.len(), 1);
        l.clear();
        assert!(l.is_empty());
    }

    #[test]
    fn whole_marker_matches_case_and_punctuation_insensitively() {
        let mut l = TitleLedger::new();
        l.register("SECRET_TOKEN_ABC123");
        assert_eq!(l.find_in("x \"secret token abc123\" y"), Some(0));
        assert_eq!(l.find_in("...Secret-Token-ABC123..."), Some(0));
        assert_eq!(l.find_in("nothing here"), None);
        assert_eq!(l.find_in(""), None);
    }

    #[test]
    fn fragments_of_long_markers_match_but_short_overlaps_do_not() {
        let mut l = TitleLedger::new();
        l.register("Reset your password - evil.example/reset?token=abc");
        // 20+ normalised characters of the title (about three words): caught.
        assert_eq!(l.find_in("see evil example reset token"), Some(0));
        assert_eq!(l.find_in("Reset your password: evil"), Some(0));
        // A shorter overlap is not (a single common word like \"instructions\" must not fire).
        assert_eq!(l.find_in("resetyourpass"), None);
        // A common word on its own is not a leak.
        assert_eq!(l.find_in("password field [hidden]"), None);
        assert_eq!(l.find_in("reset"), None);
    }

    #[test]
    fn markers_at_the_minimum_length_need_a_whole_match() {
        let mut l = TitleLedger::new();
        l.register("abcdefgh");
        assert_eq!(l.find_in("xx abcdefg yy"), None);
        assert_eq!(l.find_in("xx abcdefgh yy"), Some(0));
    }

    #[test]
    fn unicode_markers_work() {
        let mut l = TitleLedger::new();
        l.register("Пароль: Сброс доступа");
        assert_eq!(l.find_in("... пароль сброс доступа ..."), Some(0));
    }

    #[test]
    fn json_strings_collects_keys_and_values_and_skips_blobs() {
        let blob = "A".repeat(40 * 1024);
        let v = serde_json::json!({"k": ["v1", {"k2": "v2"}], "img": blob, "n": 3});
        let mut out = Vec::new();
        json_strings(&v, &mut out);
        out.sort();
        assert_eq!(out, vec!["img", "k", "k2", "n", "v1", "v2"]);
    }
}
