//! A parsed line for the viewer. Built only from the known schema fields; unknown keys are
//! ignored, so a hand-edited file cannot smuggle content into the UI.

use std::fmt;

use serde::Deserialize;

const CAP_TOKEN: usize = 48;
const CAP_LABEL: usize = 60;
const CAP_APP: usize = 128;

/// One audit line. Content-free by construction: ids, digests, fixed tokens and the sanitised
/// label only.
#[derive(Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub ts_ms: u64,
    pub session_id: String,
    pub task: Option<u64>,
    pub seq: u64,
    pub event: String,
    pub plan_digest8: Option<String>,
    pub action_digest8: Option<String>,
    pub step: Option<u16>,
    pub op: Option<String>,
    pub target_role: Option<String>,
    pub target_label: Option<String>,
    pub app_id: Option<String>,
    pub tier: Option<String>,
    pub decision: Option<String>,
    pub result_code: Option<String>,
    pub text_len: Option<u32>,
    pub segments_removed: Option<u32>,
}

impl fmt::Debug for AuditEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AuditEntry(#{} {} task {:?} step {:?} label_len {})",
            self.seq,
            self.event,
            self.task,
            self.step,
            self.target_label.as_ref().map_or(0, |l| l.chars().count())
        )
    }
}

#[derive(Deserialize)]
struct Raw {
    v: u32,
    event: String,
    #[serde(default)]
    ts: u64,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    task: Option<u64>,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    plan_digest8: Option<String>,
    #[serde(default)]
    action_digest8: Option<String>,
    #[serde(default)]
    step: Option<u16>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    target_role: Option<String>,
    #[serde(default)]
    target_label: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    result_code: Option<String>,
    #[serde(default)]
    text_len: Option<u32>,
    #[serde(default)]
    segments_removed: Option<u32>,
}

/// Drops control / invisible characters and caps the length.
fn clip(s: String, n: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'))
        .take(n)
        .collect()
}

impl AuditEntry {
    /// `None` = not a valid v1 line (corrupt, truncated, invalid UTF-8, other version).
    pub(crate) fn parse(line: &[u8]) -> Option<Self> {
        let r: Raw = serde_json::from_slice(line).ok()?;
        if r.v != crate::SCHEMA_VERSION {
            return None;
        }
        let tok = |s: Option<String>| s.map(|s| clip(s, CAP_TOKEN));
        Some(Self {
            ts_ms: r.ts,
            session_id: clip(r.session_id, CAP_TOKEN),
            task: r.task,
            seq: r.seq,
            event: clip(r.event, CAP_TOKEN),
            plan_digest8: tok(r.plan_digest8),
            action_digest8: tok(r.action_digest8),
            step: r.step,
            op: tok(r.op),
            target_role: tok(r.target_role),
            target_label: r.target_label.map(|s| clip(s, CAP_LABEL)),
            app_id: r.app_id.map(|s| clip(s, CAP_APP)),
            tier: tok(r.tier),
            decision: tok(r.decision),
            result_code: tok(r.result_code),
            text_len: r.text_len,
            segments_removed: r.segments_removed,
        })
    }
}
