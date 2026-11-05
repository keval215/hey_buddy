//! Agent activity log commands (S2-12): the viewer's page, manual export and the confirmed
//! clear, as thin, content-free wrappers over the `audit` crate. Errors are fixed tokens.

use std::path::PathBuf;

use audit::{AuditEntry, AuditPage, LogError};

use crate::api::{AuditEntryDto, AuditPageDto, IssueCode};
use crate::models_path::{check_filesystem, check_lexical};

/// Most entries one `get_audit_entries` call returns.
pub const DEFAULT_PAGE: u32 = 50;

/// Why an audit command failed (`Display` is the token the UI maps to text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuditCmdError {
    /// The log is not set up (the agent stays off too).
    #[error("unavailable")]
    Unavailable,
    /// Clear without `confirm: true`.
    #[error("needs_confirm")]
    NeedsConfirm,
    /// The export destination is not acceptable.
    #[error("invalid_path")]
    InvalidPath,
    /// The export destination already exists (never overwritten).
    #[error("exists")]
    Exists,
    #[error("io_error")]
    Io,
    /// An agent task is active or an intent / action is outstanding (S2-13).
    #[error("busy")]
    Busy,
}

impl From<LogError> for AuditCmdError {
    fn from(e: LogError) -> Self {
        match e {
            LogError::Unavailable | LogError::Symlink => Self::Unavailable,
            LogError::NeedsConfirm => Self::NeedsConfirm,
            LogError::Exists => Self::Exists,
            LogError::InvalidPath => Self::InvalidPath,
            LogError::Io => Self::Io,
        }
    }
}

impl From<AuditEntry> for AuditEntryDto {
    fn from(e: AuditEntry) -> Self {
        Self {
            ts_ms: e.ts_ms,
            session_id: e.session_id,
            task: e.task,
            seq: e.seq,
            event: e.event,
            plan_digest8: e.plan_digest8,
            action_digest8: e.action_digest8,
            step: e.step,
            op: e.op,
            target_role: e.target_role,
            target_label: e.target_label,
            app_id: e.app_id,
            tier: e.tier,
            decision: e.decision,
            result_code: e.result_code,
            text_len: e.text_len,
            segments_removed: e.segments_removed,
        }
    }
}

pub(crate) fn page_dto(p: AuditPage, over_limit: bool, ready: bool) -> AuditPageDto {
    AuditPageDto {
        entries: p.entries.into_iter().map(Into::into).collect(),
        corrupt_lines: u32::try_from(p.corrupt_lines).unwrap_or(u32::MAX),
        next_cursor: p.next_cursor.and_then(|c| u32::try_from(c).ok()),
        segments: p.segments,
        over_limit,
        ready,
    }
}

/// The export destination as typed: an absolute path to a NEW file, validated like the models
/// folder (no traversal, UNC / network, device names, control characters, system directories),
/// whose parent is an existing real directory and which does not exist yet.
pub fn validate_export_path(input: &str) -> Result<PathBuf, AuditCmdError> {
    let clean = check_lexical(input).map_err(|_: IssueCode| AuditCmdError::InvalidPath)?;
    if clean.is_empty() {
        return Err(AuditCmdError::InvalidPath);
    }
    let dest = PathBuf::from(&clean);
    dest.file_name().ok_or(AuditCmdError::InvalidPath)?;
    let parent = dest.parent().ok_or(AuditCmdError::InvalidPath)?;
    let parent_text = parent.to_str().ok_or(AuditCmdError::InvalidPath)?;
    // Filesystem check of the folder: a real directory, not a link, not a system directory.
    check_filesystem(parent_text).map_err(|_| AuditCmdError::InvalidPath)?;
    match std::fs::symlink_metadata(parent) {
        Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {}
        _ => return Err(AuditCmdError::InvalidPath),
    }
    // Never overwrite anything (a dangling link counts as existing).
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => Err(AuditCmdError::Exists),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(dest),
        Err(_) => Err(AuditCmdError::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::path::Path;

    #[test]
    fn export_path_rules() {
        let d = tempfile::tempdir().unwrap();
        let p = |s: &str| d.path().join(s).to_string_lossy().into_owned();
        // Good: a new file in an existing folder.
        let ok = validate_export_path(&p("log-export.jsonl")).unwrap();
        assert!(ok.ends_with("log-export.jsonl"));
        // Existing file: refused, never overwritten.
        std::fs::write(d.path().join("there.jsonl"), b"keep").unwrap();
        assert_eq!(
            validate_export_path(&p("there.jsonl")),
            Err(AuditCmdError::Exists)
        );
        assert_eq!(
            std::fs::read(d.path().join("there.jsonl")).unwrap(),
            b"keep"
        );
        // Missing parent, a file as parent.
        assert_eq!(
            validate_export_path(&p("no/such/x.jsonl")),
            Err(AuditCmdError::InvalidPath)
        );
        assert_eq!(
            validate_export_path(&p("there.jsonl/x.jsonl")),
            Err(AuditCmdError::InvalidPath)
        );
        let base = d.path().to_string_lossy().into_owned();
        for bad in [
            "".to_string(),
            "   ".to_string(),
            "relative/x.jsonl".to_string(),
            "x.jsonl".to_string(),
            format!("{base}/../x.jsonl"),
            format!("{base}/a/../../x.jsonl"),
            "\\\\server\\share\\x.jsonl".to_string(),
            "//server/share/x.jsonl".to_string(),
            "\\\\?\\C:\\x.jsonl".to_string(),
            "C:x.jsonl".to_string(),
            format!("{base}/x\n.jsonl"),
            format!("{base}/x\u{202e}.jsonl"),
            format!("{base}/x:stream"),
            format!("{base}/CON"),
            format!("{base}/bad|name"),
            format!("{base}/trailing."),
            "/".to_string(),
            "/etc/x.jsonl".to_string(),
            "/usr/bin/x.jsonl".to_string(),
            "C:\\Windows\\System32\\x.jsonl".to_string(),
            format!("/{}", "a".repeat(2000)),
        ] {
            assert_eq!(
                validate_export_path(&bad),
                Err(AuditCmdError::InvalidPath),
                "{bad:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_parent_or_target_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let p = |x: &Path| x.join("x.jsonl").to_string_lossy().into_owned();
        assert_eq!(
            validate_export_path(&p(&link)),
            Err(AuditCmdError::InvalidPath)
        );
        // A dangling link as the destination counts as existing.
        let dangling = real.join("d.jsonl");
        std::os::unix::fs::symlink(d.path().join("nowhere"), &dangling).unwrap();
        assert_eq!(
            validate_export_path(&dangling.to_string_lossy()),
            Err(AuditCmdError::Exists)
        );
    }
}
