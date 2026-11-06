//! Where the app writes things, and the only code that erases them (S3-6). `docs/data.md` lists
//! the same files for people; a test keeps the two in step.
//!
//! Two roots, both given by the shell and never by the UI: the app config directory
//! (`settings.json`, `deny_list.json`) and the app data directory (everything else; on Windows
//! the two are the same folder).
//!
//! Deleting is deliberately narrow:
//! - A kind owns a fixed list of names ([`DataItem`]); nothing else is ever touched, so extra
//!   files a user keeps in these folders survive. Names are matched exactly (no case folding,
//!   no globbing) and never come from the UI.
//! - Each target is built below its root from plain names only, and every folder on the way is
//!   checked with `symlink_metadata`: a symlink or Windows reparse point / junction is NEVER
//!   followed and never deleted through; it is refused and counted as `failed`.
//! - Below a kind's own folder (for example `updates/kept`) the walk is manual: links inside
//!   are skipped and counted, regular files and folders are removed.
//! - A missing folder is fine (nothing to delete). A partial failure is reported as counts.
//!
//! The audit log is not removed here: the audit sink owns that file (append-only by design) and
//! clears it itself (new segment starts with a `cleared` record, then the old ones go), see
//! `Buddy::data_delete`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use voice::registry::{ModelEntry, Registry};

use crate::api::{DataDeleteDto, DataKind, DataKindDto};
use crate::download::{expected, DOWNLOADS_DIR};
use crate::logging;
use crate::update::{KEPT_DIR, STAGED_ENVELOPE, STAGED_FILE};

/// Which root a path is below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The app config directory.
    Config,
    /// The app data directory.
    Data,
}

/// What a data item is, for `docs/data.md` and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataItem {
    pub kind: DataKind,
    pub root: Root,
    /// Relative to the root, `/` separated. A trailing `/` is a folder.
    pub path: &'static str,
    /// May hold anything personal to the user? (Never screenshots, audio or typed values.)
    pub personal: bool,
}

/// Every path the app writes. Keep in step with `docs/data.md` (a test checks it).
pub const DATA_ITEMS: &[DataItem] = &[
    item(DataKind::Settings, Root::Config, "settings.json", false),
    item(DataKind::DenyList, Root::Config, "deny_list.json", false),
    item(DataKind::AuditLog, Root::Data, "audit/", true),
    item(DataKind::Models, Root::Data, "models/", false),
    item(DataKind::Updates, Root::Data, "updates/", false),
    item(DataKind::Logs, Root::Data, "logs/", false),
];

const fn item(kind: DataKind, root: Root, path: &'static str, personal: bool) -> DataItem {
    DataItem {
        kind,
        root,
        path,
        personal,
    }
}

impl DataKind {
    pub const ALL: [DataKind; 6] = [
        DataKind::Settings,
        DataKind::DenyList,
        DataKind::AuditLog,
        DataKind::Models,
        DataKind::Updates,
        DataKind::Logs,
    ];

    /// The confirmation word the delete command must be given for this kind. A call without
    /// exactly this word does nothing.
    pub fn confirm_token(self) -> &'static str {
        match self {
            DataKind::Settings => "delete-settings",
            DataKind::DenyList => "delete-deny-list",
            DataKind::AuditLog => "delete-audit-log",
            DataKind::Models => "delete-models",
            DataKind::Updates => "delete-updates",
            DataKind::Logs => "delete-logs",
        }
    }
}

/// Why a data command failed (`Display` is the token the UI maps to text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DataError {
    /// The confirmation word was missing or wrong.
    #[error("needs_confirm")]
    NeedsConfirm,
    /// A task, turn, download or update is running.
    #[error("busy")]
    Busy,
    /// The folder is not set up (or the audit log is not available).
    #[error("unavailable")]
    Unavailable,
    #[error("io_error")]
    Io,
}

/// A name that is one plain path component: no separators, no `.`/`..`, no drive or stream
/// markers, no control characters, no trailing dot or space (Windows).
pub fn is_plain_name(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && n.len() <= 128
        && !n.contains(['/', '\\', ':', '\0'])
        && !n.chars().any(char::is_control)
        && !n.ends_with(['.', ' '])
}

#[cfg(windows)]
fn link_like(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_type().is_symlink() || md.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn link_like(md: &fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

/// What a kind erases: a folder below a root, and the exact names inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: Root,
    /// Plain folder names from the root down to the folder (empty = the root itself).
    pub dir: Vec<&'static str>,
    /// Exact names to remove inside that folder.
    pub names: Vec<String>,
}

fn first_component(rel: &str) -> Option<&str> {
    rel.split('/').next().filter(|c| is_plain_name(c))
}

/// Top-level names a model entry may leave in the models folder.
fn model_names(m: &ModelEntry, out: &mut Vec<String>) {
    for (rel, _) in expected(m).unwrap_or_default() {
        if let Some(c) = first_component(&rel) {
            if !out.iter().any(|o| o == c) {
                out.push(c.to_string());
            }
        }
    }
}

/// What `kind` erases. `None` for the audit log (the audit sink does that).
pub fn target(kind: DataKind, reg: &Registry) -> Option<Target> {
    let t = |root, dir: &[&'static str], names: Vec<String>| Target {
        root,
        dir: dir.to_vec(),
        names,
    };
    Some(match kind {
        DataKind::Settings => t(
            Root::Config,
            &[],
            vec!["settings.json".into(), "settings.json.tmp".into()],
        ),
        DataKind::DenyList => t(
            Root::Config,
            &[],
            vec!["deny_list.json".into(), "deny_list.json.tmp".into()],
        ),
        DataKind::AuditLog => return None,
        DataKind::Models => {
            let mut names = vec![DOWNLOADS_DIR.to_string()];
            for m in &reg.models {
                model_names(m, &mut names);
            }
            t(Root::Data, &["models"], names)
        }
        // `state.json` (the highest manifest serial seen, the anti-rollback memory) stays: erasing
        // it would let an old signed manifest be replayed. It holds two numbers and no content.
        DataKind::Updates => t(
            Root::Data,
            &["updates"],
            vec![
                STAGED_FILE.into(),
                STAGED_ENVELOPE.into(),
                format!("{STAGED_ENVELOPE}.tmp"),
                KEPT_DIR.into(),
                update::launch::RUN_DIR.into(),
                DOWNLOADS_DIR.into(),
            ],
        ),
        DataKind::Logs => t(Root::Data, &["logs"], logging::log_file_names()),
    })
}

/// Running totals of one deletion.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub removed: u32,
    pub failed: u32,
}

impl Tally {
    fn add(&mut self, o: Tally) {
        self.removed = self.removed.saturating_add(o.removed);
        self.failed = self.failed.saturating_add(o.failed);
    }
}

/// Deepest folder nesting the walk goes into (our own folders are shallow).
const MAX_DEPTH: u32 = 16;

/// Walk `root` + `dir` component by component, refusing links. `Ok(None)` = a folder is
/// missing (nothing to do). `Err` = a link or a non-folder is in the way.
fn resolve_dir(root: &Path, dir: &[&str]) -> Result<Option<PathBuf>, ()> {
    let mut cur = root.to_path_buf();
    for c in dir {
        if !is_plain_name(c) {
            return Err(());
        }
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(md) if link_like(&md) || !md.is_dir() => return Err(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        }
    }
    Ok(Some(cur))
}

/// Remove `names` inside `root/dir`. See the module docs for the rules.
pub fn delete_named(root: &Path, dir: &[&str], names: &[String]) -> Tally {
    let mut tally = Tally::default();
    let parent = match resolve_dir(root, dir) {
        Ok(Some(p)) => p,
        Ok(None) => return tally,
        Err(()) => {
            tally.failed = 1;
            return tally;
        }
    };
    // Belt and braces: the resolved folder is really below the root.
    if let (Ok(r), Ok(p)) = (fs::canonicalize(root), fs::canonicalize(&parent)) {
        if !p.starts_with(&r) {
            tally.failed = 1;
            return tally;
        }
    }
    for n in names {
        if !is_plain_name(n) {
            tally.failed += 1;
            continue;
        }
        let p = parent.join(n);
        debug_assert!(p.starts_with(root) && !p.components().any(|c| c == Component::ParentDir));
        match fs::symlink_metadata(&p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => tally.failed += 1,
            Ok(md) if link_like(&md) => tally.failed += 1,
            Ok(md) if md.is_dir() => tally.add(remove_tree(&p, 0)),
            Ok(_) => match fs::remove_file(&p) {
                Ok(()) => tally.removed += 1,
                Err(_) => tally.failed += 1,
            },
        }
    }
    tally
}

/// Remove a folder of ours and what is in it, never following or deleting a link.
fn remove_tree(dir: &Path, depth: u32) -> Tally {
    let mut t = Tally::default();
    if depth >= MAX_DEPTH {
        t.failed = 1;
        return t;
    }
    // Re-check at entry that `dir` itself is a real folder, not a link swapped in since the
    // caller looked (fail closed: do not even list it).
    match fs::symlink_metadata(dir) {
        Ok(md) if !link_like(&md) && md.is_dir() => {}
        _ => {
            t.failed = 1;
            return t;
        }
    }
    let Ok(rd) = fs::read_dir(dir) else {
        t.failed = 1;
        return t;
    };
    for e in rd {
        let Ok(e) = e else {
            t.failed += 1;
            continue;
        };
        let p = e.path();
        match fs::symlink_metadata(&p) {
            Ok(md) if link_like(&md) => t.failed += 1,
            Ok(md) if md.is_dir() => t.add(remove_tree(&p, depth + 1)),
            Ok(_) => match fs::remove_file(&p) {
                Ok(()) => t.removed += 1,
                Err(_) => t.failed += 1,
            },
            Err(_) => t.failed += 1,
        }
    }
    if fs::remove_dir(dir).is_err() {
        t.failed += 1;
    }
    t
}

/// File count and byte total below `p` (links are not followed or counted).
fn measure(p: &Path, depth: u32) -> (u32, u64) {
    let Ok(md) = fs::symlink_metadata(p) else {
        return (0, 0);
    };
    if link_like(&md) {
        return (0, 0);
    }
    if !md.is_dir() {
        return (1, md.len());
    }
    if depth >= MAX_DEPTH {
        return (0, 0);
    }
    let mut tot = (0u32, 0u64);
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let (f, b) = measure(&e.path(), depth + 1);
            tot = (tot.0.saturating_add(f), tot.1.saturating_add(b));
        }
    }
    tot
}

/// Audit segment names (`audit-000001.jsonl`), the same shape the audit crate writes.
fn is_audit_segment(n: &str) -> bool {
    n.strip_prefix("audit-")
        .and_then(|r| r.strip_suffix(".jsonl"))
        .is_some_and(|d| (6..=9).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()))
}

/// The two folders the app writes into, as the shell gave them.
#[derive(Debug, Clone, Default)]
pub struct DataLocations {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

impl DataLocations {
    fn root(&self, r: Root) -> Option<&Path> {
        match r {
            Root::Config => self.config_dir.as_deref(),
            Root::Data => self.data_dir.as_deref(),
        }
    }

    /// Counts and sizes for every kind (read-only; links are not followed).
    pub fn overview(&self, reg: &Registry) -> Vec<DataKindDto> {
        DataKind::ALL
            .iter()
            .map(|&kind| {
                let (files, bytes) = self.measure_kind(kind, reg);
                DataKindDto { kind, files, bytes }
            })
            .collect()
    }

    fn measure_kind(&self, kind: DataKind, reg: &Registry) -> (u32, u64) {
        let sum = |a: (u32, u64), b: (u32, u64)| (a.0.saturating_add(b.0), a.1.saturating_add(b.1));
        if kind == DataKind::AuditLog {
            let Some(root) = self.root(Root::Data) else {
                return (0, 0);
            };
            let Ok(Some(dir)) = resolve_dir(root, &["audit"]) else {
                return (0, 0);
            };
            let Ok(rd) = fs::read_dir(dir) else {
                return (0, 0);
            };
            return rd
                .flatten()
                .filter(|e| e.file_name().to_str().is_some_and(is_audit_segment))
                .fold((0, 0), |acc, e| sum(acc, measure(&e.path(), 0)));
        }
        let Some(t) = target(kind, reg) else {
            return (0, 0);
        };
        let Some(root) = self.root(t.root) else {
            return (0, 0);
        };
        let Ok(Some(dir)) = resolve_dir(root, &t.dir) else {
            return (0, 0);
        };
        t.names
            .iter()
            .filter(|n| is_plain_name(n))
            .fold((0, 0), |acc, n| sum(acc, measure(&dir.join(n), 0)))
    }

    /// Erase `kind` (not the audit log). `Unavailable` when the root is not known.
    pub fn delete(&self, kind: DataKind, reg: &Registry) -> Result<DataDeleteDto, DataError> {
        let t = target(kind, reg).ok_or(DataError::Unavailable)?;
        let root = self.root(t.root).ok_or(DataError::Unavailable)?;
        let tally = delete_named(root, &t.dir, &t.names);
        Ok(DataDeleteDto {
            kind,
            removed: tally.removed,
            failed: tally.failed,
            restart_needed: matches!(kind, DataKind::Settings | DataKind::DenyList),
        })
    }
}

#[cfg(test)]
mod tests;
