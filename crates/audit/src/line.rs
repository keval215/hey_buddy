//! The on-disk line. Built ONLY from a `policy::AuditRecord` (or the fixed `cleared` event), so
//! typed text, hashes, transcripts, plan JSON, prompts, screen text, clipboard and screenshots
//! have no way in. Strings are capped again here (defence in depth).

use policy::{AuditRecord, MAX_AUDIT_LABEL_CHARS};
use serde::Serialize;

/// Schema version written as `"v"`.
pub const SCHEMA_VERSION: u32 = 1;
/// Longest line (bytes, without the newline).
pub const MAX_LINE_BYTES: usize = 4096;
/// Longest app id kept.
pub const MAX_APP_ID_CHARS: usize = 128;

#[derive(Serialize)]
struct Line<'a> {
    v: u32,
    ts: u64,
    session_id: &'a str,
    task: Option<u64>,
    seq: u64,
    event: &'a str,
    plan_digest8: Option<String>,
    action_digest8: Option<String>,
    step: Option<u16>,
    op: Option<&'a str>,
    target_role: Option<&'a str>,
    target_label: Option<String>,
    app_id: Option<String>,
    tier: Option<&'a str>,
    decision: Option<&'a str>,
    result_code: Option<&'a str>,
    text_len: Option<u32>,
    /// Only on `cleared` lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    segments_removed: Option<u32>,
}

fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Exactly 8 lowercase hex digits, else dropped.
fn digest8(d: &Option<String>) -> Option<String> {
    d.as_ref()
        .filter(|s| s.len() <= 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| s.to_ascii_lowercase())
}

/// One line for `rec` (no trailing newline). `segments_removed` is for the `cleared` event.
pub(crate) fn encode(
    session_id: &str,
    seq: u64,
    rec: &AuditRecord,
    segments_removed: Option<u32>,
) -> Vec<u8> {
    let mut line = Line {
        v: SCHEMA_VERSION,
        ts: rec.ts_ms,
        session_id,
        task: rec.task,
        seq,
        event: rec.event,
        plan_digest8: digest8(&rec.plan_digest8),
        action_digest8: digest8(&rec.action_digest8),
        step: rec.step,
        op: rec.op,
        target_role: rec.target_role,
        target_label: rec
            .target_label
            .as_deref()
            .map(|l| cap(l, MAX_AUDIT_LABEL_CHARS)),
        app_id: rec.app_id.as_deref().map(|a| cap(a, MAX_APP_ID_CHARS)),
        tier: rec.tier,
        decision: rec.decision,
        result_code: rec.result_code,
        text_len: rec.text_len,
        segments_removed,
    };
    // serde_json escapes every control character (so no raw `\n` / `\r`), quote and backslash.
    // U+2028 / U+2029 are escaped too (`to_vec`), for viewers that treat them as line breaks.
    let mut out = to_vec(&line);
    // Over-long: shorten the free-text fields BEFORE writing; never drop the record.
    let mut guard = 0;
    while out.len() > MAX_LINE_BYTES && guard < 16 {
        guard += 1;
        let half = |s: &mut Option<String>| {
            if let Some(v) = s {
                let n = v.chars().count() / 2;
                *v = v.chars().take(n).collect();
            }
        };
        half(&mut line.target_label);
        half(&mut line.app_id);
        out = to_vec(&line);
    }
    if out.len() > MAX_LINE_BYTES {
        line.target_label = None;
        line.app_id = None;
        out = to_vec(&line);
    }
    out
}

fn to_vec(l: &Line<'_>) -> Vec<u8> {
    // Plain data: cannot fail.
    let raw = serde_json::to_vec(l).unwrap_or_default();
    // U+2028 / U+2029 are the UTF-8 bytes E2 80 A8 / E2 80 A9; they only occur inside strings.
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0xE2
            && raw.get(i + 1) == Some(&0x80)
            && matches!(raw.get(i + 2), Some(0xA8 | 0xA9))
        {
            out.extend_from_slice(if raw[i + 2] == 0xA8 {
                b"\\u2028"
            } else {
                b"\\u2029"
            });
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}
