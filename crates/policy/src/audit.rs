//! Audit interface (ADR 0003 section 13). The gate writes an INTENT record before it mints an
//! `ApprovedAction`; if that append fails the mint is refused (fail closed). The real
//! append-only file sink is S2-12; here the trait, the record, `NullAudit` and `MemoryAudit`.
//!
//! NEVER in a record: typed text, its hash, transcripts, plan JSON, prompts, screen text.
//! `target_label` is sanitised, at most 60 characters, and replaced by `[redacted]` when it
//! contains any 4-character substring of the typed text or the element/page is sensitive.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::norm;

pub const MAX_AUDIT_LABEL_CHARS: usize = 60;
pub const REDACTED_LABEL: &str = "[redacted]";

/// The append failed. Fixed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("audit_append_failed")]
pub struct AuditError;

/// One audit line (fields per ADR 0003 section 13; `session_id` is added by the file sink).
/// `Debug` prints everything except the label text.
#[derive(Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub ts_ms: u64,
    pub task: Option<u64>,
    /// Per gate, strictly increasing.
    pub seq: u64,
    pub event: &'static str,
    pub plan_digest8: Option<String>,
    pub action_digest8: Option<String>,
    pub step: Option<u16>,
    pub op: Option<&'static str>,
    pub target_role: Option<&'static str>,
    pub target_label: Option<String>,
    pub app_id: Option<String>,
    pub tier: Option<&'static str>,
    pub decision: Option<&'static str>,
    pub result_code: Option<&'static str>,
    pub text_len: Option<u32>,
}

impl AuditRecord {
    pub(crate) fn blank(event: &'static str) -> Self {
        Self {
            ts_ms: 0,
            task: None,
            seq: 0,
            event,
            plan_digest8: None,
            action_digest8: None,
            step: None,
            op: None,
            target_role: None,
            target_label: None,
            app_id: None,
            tier: None,
            decision: None,
            result_code: None,
            text_len: None,
        }
    }
}

impl fmt::Debug for AuditRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AuditRecord(#{} {} task {:?} step {:?} op {:?} tier {:?} decision {:?} result {:?} label_len {})",
            self.seq,
            self.event,
            self.task,
            self.step,
            self.op,
            self.tier,
            self.decision,
            self.result_code,
            self.target_label.as_ref().map_or(0, |l| l.chars().count())
        )
    }
}

pub trait AuditSink: Send + Sync {
    /// Write-ahead record of an action about to be minted. `Err` makes the gate refuse.
    fn intent(&self, r: &AuditRecord) -> Result<(), AuditError>;
    /// Any other event (decisions, refusals, results, task end). Best effort.
    fn record(&self, r: &AuditRecord);
    /// The kill switch fired (ADR 0003 section 9).
    fn killed(&self, r: &AuditRecord);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullAudit;

impl AuditSink for NullAudit {
    fn intent(&self, _r: &AuditRecord) -> Result<(), AuditError> {
        Ok(())
    }
    fn record(&self, _r: &AuditRecord) {}
    fn killed(&self, _r: &AuditRecord) {}
}

/// Keeps records in memory (tests). `fail_intents(true)` makes `intent` fail.
#[derive(Debug, Default)]
pub struct MemoryAudit {
    intents: Mutex<Vec<AuditRecord>>,
    records: Mutex<Vec<AuditRecord>>,
    kills: Mutex<Vec<AuditRecord>>,
    fail: AtomicBool,
}

impl MemoryAudit {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn fail_intents(&self, on: bool) {
        self.fail.store(on, Ordering::SeqCst);
    }
    pub fn intents(&self) -> Vec<AuditRecord> {
        self.intents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn records(&self) -> Vec<AuditRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn kills(&self) -> Vec<AuditRecord> {
        self.kills.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Every record of every kind, in no particular cross-kind order.
    pub fn all(&self) -> Vec<AuditRecord> {
        let mut v = self.intents();
        v.extend(self.records());
        v.extend(self.kills());
        v
    }
}

impl AuditSink for MemoryAudit {
    fn intent(&self, r: &AuditRecord) -> Result<(), AuditError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(AuditError);
        }
        self.intents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(r.clone());
        Ok(())
    }
    fn record(&self, r: &AuditRecord) {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(r.clone());
    }
    fn killed(&self, r: &AuditRecord) {
        self.kills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(r.clone());
    }
}

/// Sanitises a target label for the audit line: at most 60 characters, and `[redacted]` when the
/// element or page is sensitive or the label shares any 4-character substring with the typed
/// text (compared case-, accent-, space- and invisible-insensitively).
pub fn audit_label(label: &str, typed: Option<&str>, sensitive: bool) -> String {
    if sensitive {
        return REDACTED_LABEL.to_owned();
    }
    if let Some(t) = typed {
        if shares_substring(label, t, 4) {
            return REDACTED_LABEL.to_owned();
        }
    }
    norm::sanitise(label)
        .chars()
        .take(MAX_AUDIT_LABEL_CHARS)
        .collect()
}

fn shares_substring(label: &str, typed: &str, n: usize) -> bool {
    let forms = |s: &str| {
        let p = norm::provenance(s);
        let packed: String = p.chars().filter(|c| !c.is_whitespace()).collect();
        [p, packed]
    };
    let lf = forms(label);
    for t in forms(typed) {
        let cs: Vec<char> = t.chars().collect();
        if cs.len() < n {
            continue;
        }
        for w in cs.windows(n) {
            let needle: String = w.iter().collect();
            if lf.iter().any(|l| l.contains(&needle)) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_rules() {
        assert_eq!(audit_label("Name", None, false), "Name");
        assert_eq!(audit_label("Name", None, true), REDACTED_LABEL);
        assert_eq!(
            audit_label(
                "Field PRIVATE_MARKER here",
                Some("xx private_marker yy"),
                false
            ),
            REDACTED_LABEL
        );
        // Three shared characters are fine; four are not.
        assert_eq!(audit_label("abc", Some("abc"), false), "abc");
        assert_eq!(audit_label("abcd", Some("zabcdz"), false), REDACTED_LABEL);
        assert_eq!(
            audit_label("pri vate", Some("private"), false),
            REDACTED_LABEL
        );
        let long = "x".repeat(200);
        assert_eq!(audit_label(&long, None, false).chars().count(), 60);
    }
}
