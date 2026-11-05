//! The deny-list editor's logic (S2-7): add an app, remove an app, restore the built-in
//! entries. State is the same `{add, remove}` shape as `deny_list.json`, written atomically
//! after every change. Rules:
//! - an id is `[A-Za-z0-9._-]{1,128}`, stored lowercase;
//! - a user addition can be removed freely; a BUILT-IN entry only with `confirm`;
//! - the effective list is never empty: a change that would empty it is refused;
//! - a file that is unreadable, invalid or would empty the list falls back to the built-in
//!   list and says so (the file is replaced by the first successful change).
//!
//! Errors are fixed tokens; no ids or paths in them.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use capture::deny::{config_path, DenyListError, UserConfig};
use capture::DenyList;

use crate::api::{DenyEntryDto, DenyListDto};
use crate::settings::write_atomic;

/// Longest accepted app id.
pub const MAX_ID_LEN: usize = 128;

/// Why an edit was refused (`Display` is the token the UI maps to text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DenyEditError {
    #[error("invalid_id")]
    InvalidId,
    #[error("already_listed")]
    AlreadyListed,
    #[error("not_found")]
    NotFound,
    /// Removing a built-in entry (or resetting) without the confirm flag.
    #[error("needs_confirm")]
    NeedsConfirm,
    /// The change would leave nothing on the list.
    #[error("would_be_empty")]
    WouldBeEmpty,
    #[error("io_error")]
    Io,
    /// The shell has not told the core where the config folder is.
    #[error("unavailable")]
    Unavailable,
}

/// `[A-Za-z0-9._-]{1,128}`, returned lowercase.
pub fn validate_app_id(id: &str) -> Result<String, DenyEditError> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > MAX_ID_LEN
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(DenyEditError::InvalidId);
    }
    Ok(id.to_ascii_lowercase())
}

/// Lookup form of an id to remove: lenient (hand-edited entries may not match the add
/// pattern) but still bounded.
fn lookup_id(id: &str) -> Result<String, DenyEditError> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_ID_LEN || id.chars().any(char::is_control) {
        return Err(DenyEditError::InvalidId);
    }
    Ok(id.to_ascii_lowercase())
}

fn token(e: &DenyListError) -> &'static str {
    match e {
        DenyListError::Unreadable => "unreadable",
        DenyListError::Invalid => "invalid",
        DenyListError::WouldBeEmpty => "would_be_empty",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Sets {
    add: BTreeSet<String>,
    remove: BTreeSet<String>,
}

impl Sets {
    fn from_config(cfg: &UserConfig) -> Self {
        let norm = |v: &[String]| {
            v.iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        };
        Self {
            add: norm(&cfg.add),
            remove: norm(&cfg.remove),
        }
    }

    fn to_config(&self) -> UserConfig {
        UserConfig {
            add: self.add.iter().cloned().collect(),
            remove: self.remove.iter().cloned().collect(),
        }
    }
}

pub struct DenyEditor {
    dir: PathBuf,
    sets: Sets,
    /// Why the file was not used when it was loaded; cleared by the first saved change.
    load_issue: Option<&'static str>,
}

impl DenyEditor {
    /// Read `<dir>/deny_list.json`. Missing = built-in list; unreadable, invalid or
    /// list-emptying = built-in list plus a visible issue.
    pub fn load(dir: PathBuf) -> Self {
        let (cfg, mut issue) = match std::fs::read_to_string(config_path(&dir)) {
            Ok(t) => match serde_json::from_str::<UserConfig>(&t) {
                Ok(c) => (c, None),
                Err(_) => (UserConfig::default(), Some(DenyListError::Invalid)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (UserConfig::default(), None),
            Err(_) => (UserConfig::default(), Some(DenyListError::Unreadable)),
        };
        let mut sets = Sets::from_config(&cfg);
        if let Some(e) = DenyList::apply_config(&sets.to_config()).error {
            issue = Some(e);
        }
        if issue.is_some() {
            sets = Sets::default();
        }
        Self {
            dir,
            sets,
            load_issue: issue.as_ref().map(token),
        }
    }

    /// The effective list (never empty).
    pub fn list(&self) -> Arc<DenyList> {
        Arc::new(DenyList::from_config(&self.sets.to_config()))
    }

    /// Built-in entries the user removed, sorted.
    pub fn removed_defaults(&self) -> Vec<String> {
        DenyList::apply_config(&self.sets.to_config()).removed_defaults
    }

    pub fn load_issue(&self) -> Option<&'static str> {
        self.load_issue
    }

    /// Display-only notes for `Status.notices`: the banking note (always), a broken file and
    /// the built-in apps the user removed.
    pub fn notices(&self) -> Vec<String> {
        let mut n = vec![crate::setup::BANKING_NOTICE.to_string()];
        if let Some(i) = self.load_issue {
            n.push(
                match i {
                    "unreadable" => "Deny list: the config file could not be read; the built-in list is in use.",
                    "would_be_empty" => "Deny list: the config file would leave the list empty; the built-in list is in use.",
                    _ => "Deny list: the config file is invalid; the built-in list is in use.",
                }
                .to_string(),
            );
        }
        let removed = self.removed_defaults();
        if !removed.is_empty() {
            n.push(format!(
                "You removed these apps from the default deny list: {}.",
                removed.join(", ")
            ));
        }
        n
    }

    pub fn view(&self, banking_notice: &str) -> DenyListDto {
        let defaults = DenyList::defaults();
        DenyListDto {
            entries: self
                .list()
                .entries()
                .map(|id| DenyEntryDto {
                    id: id.to_string(),
                    builtin: defaults.matches(id),
                })
                .collect(),
            removed_defaults: self.removed_defaults(),
            banking_notice: banking_notice.to_string(),
            load_issue: self.load_issue.map(String::from),
        }
    }

    fn commit(&mut self, next: Sets) -> Result<(), DenyEditError> {
        if DenyList::apply_config(&next.to_config()).error.is_some() {
            return Err(DenyEditError::WouldBeEmpty);
        }
        let json = serde_json::to_vec_pretty(&next.to_config()).map_err(|_| DenyEditError::Io)?;
        write_atomic(&config_path(&self.dir), &json).map_err(|_| DenyEditError::Io)?;
        self.sets = next;
        self.load_issue = None;
        Ok(())
    }

    /// Add an app. A removed built-in entry comes back (it is dropped from `remove`).
    pub fn add(&mut self, id: &str) -> Result<(), DenyEditError> {
        let id = validate_app_id(id)?;
        if self.list().matches(&id) {
            return Err(DenyEditError::AlreadyListed);
        }
        let mut next = self.sets.clone();
        next.remove.remove(&id);
        if !DenyList::defaults().matches(&id) {
            next.add.insert(id);
        }
        self.commit(next)
    }

    /// Remove an app. A user addition goes freely. A built-in entry needs
    /// `confirm_remove_default`.
    pub fn remove(&mut self, id: &str, confirm_remove_default: bool) -> Result<(), DenyEditError> {
        let id = lookup_id(id)?;
        let mut next = self.sets.clone();
        let was_added = next.add.remove(&id);
        let builtin_active = DenyList::defaults().matches(&id) && !self.sets.remove.contains(&id);
        if builtin_active {
            if !confirm_remove_default {
                return Err(DenyEditError::NeedsConfirm);
            }
            next.remove.insert(id);
        } else if !was_added {
            return Err(DenyEditError::NotFound);
        }
        self.commit(next)
    }

    /// Bring back every removed built-in entry. User additions stay (dropping them would
    /// make the list weaker). Needs `confirm`.
    pub fn reset_to_defaults(&mut self, confirm: bool) -> Result<(), DenyEditError> {
        if !confirm {
            return Err(DenyEditError::NeedsConfirm);
        }
        let mut next = self.sets.clone();
        next.remove.clear();
        self.commit(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> (tempfile::TempDir, DenyEditor) {
        let d = tempfile::tempdir().unwrap();
        let e = DenyEditor::load(d.path().to_path_buf());
        (d, e)
    }

    fn file(d: &tempfile::TempDir) -> UserConfig {
        serde_json::from_slice(&std::fs::read(config_path(d.path())).unwrap()).unwrap()
    }

    #[test]
    fn id_validation_table() {
        let long_ok = "a".repeat(128);
        for (input, ok) in [
            ("com.bank.app", true),
            ("Bank.EXE", true),
            ("  padded.exe  ", true),
            ("a", true),
            ("a-b_c.d", true),
            (long_ok.as_str(), true),
            ("", false),
            ("   ", false),
            ("a b", false),
            ("a/b", false),
            ("a\\b", false),
            ("..\\x", false),
            ("a\nb", false),
            ("a\u{0}b", false),
            ("a\u{202E}b", false),
            ("m\u{e9}", false),
            ("*", false),
            ("a;b", false),
            ("a:b", false),
        ] {
            assert_eq!(validate_app_id(input).is_ok(), ok, "{input:?}");
        }
        assert!(validate_app_id(&"a".repeat(129)).is_err());
        assert!(validate_app_id(&"a".repeat(10_240)).is_err());
        assert_eq!(validate_app_id(" Bank.EXE ").unwrap(), "bank.exe");
    }

    #[test]
    fn add_normalises_persists_and_rejects_duplicates() {
        let (d, mut e) = editor();
        e.add("  My.Bank.EXE ").unwrap();
        assert!(e.list().matches("my.bank.exe"));
        assert_eq!(file(&d).add, vec!["my.bank.exe".to_string()]);
        assert_eq!(e.add("MY.bank.exe"), Err(DenyEditError::AlreadyListed));
        // A built-in entry is already listed.
        assert_eq!(e.add("CMD.exe"), Err(DenyEditError::AlreadyListed));
        assert_eq!(e.add("bad id"), Err(DenyEditError::InvalidId));
        // Persisted file reloads to the same list; no temp file is left behind.
        let again = DenyEditor::load(d.path().to_path_buf());
        assert!(again.list().matches("my.bank.exe"));
        assert!(again.load_issue().is_none());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn user_entries_go_freely_and_unknown_ids_are_not_found() {
        let (d, mut e) = editor();
        e.add("mine.exe").unwrap();
        e.remove("MINE.exe", false).unwrap();
        assert!(!e.list().matches("mine.exe"));
        assert!(file(&d).add.is_empty());
        assert_eq!(e.remove("mine.exe", false), Err(DenyEditError::NotFound));
        assert_eq!(e.remove("", false), Err(DenyEditError::InvalidId));
        assert_eq!(e.remove("nope.exe", true), Err(DenyEditError::NotFound));
    }

    #[test]
    fn removing_a_default_needs_confirm_and_is_reported() {
        let (d, mut e) = editor();
        assert_eq!(e.remove("cmd.exe", false), Err(DenyEditError::NeedsConfirm));
        assert!(e.list().matches("cmd.exe"));
        assert!(
            !config_path(d.path()).exists(),
            "refused edit writes nothing"
        );
        e.remove("CMD.exe", true).unwrap();
        assert!(!e.list().matches("cmd.exe"));
        assert_eq!(e.removed_defaults(), vec!["cmd.exe".to_string()]);
        assert_eq!(file(&d).remove, vec!["cmd.exe".to_string()]);
        let v = e.view("note");
        assert_eq!(v.removed_defaults, vec!["cmd.exe".to_string()]);
        assert!(v.entries.iter().all(|x| x.id != "cmd.exe"));
        assert_eq!(v.banking_notice, "note");
        // Removing it again: it is gone already.
        assert_eq!(e.remove("cmd.exe", true), Err(DenyEditError::NotFound));
        // Adding it back restores it and clears the removal.
        e.add("cmd.exe").unwrap();
        assert!(e.list().matches("cmd.exe"));
        assert!(file(&d).remove.is_empty() && file(&d).add.is_empty());
    }

    #[test]
    fn a_default_that_is_also_added_still_needs_confirm() {
        // Hand-edited file: a built-in id in `add`.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(config_path(d.path()), br#"{"add":["cmd.exe"],"remove":[]}"#).unwrap();
        let mut e = DenyEditor::load(d.path().to_path_buf());
        assert_eq!(e.remove("cmd.exe", false), Err(DenyEditError::NeedsConfirm));
        e.remove("cmd.exe", true).unwrap();
        assert!(!e.list().matches("cmd.exe"));
        assert!(file(&d).add.is_empty());
    }

    #[test]
    fn the_list_is_never_empty() {
        let (d, mut e) = editor();
        let all: Vec<String> = DenyList::defaults().entries().map(String::from).collect();
        let (last, rest) = all.split_last().unwrap();
        for id in rest {
            e.remove(id, true).unwrap();
        }
        assert_eq!(e.list().len(), 1);
        let before = std::fs::read(config_path(d.path())).unwrap();
        assert_eq!(e.remove(last, true), Err(DenyEditError::WouldBeEmpty));
        assert!(e.list().matches(last));
        assert_eq!(std::fs::read(config_path(d.path())).unwrap(), before);
        // With one user entry the last built-in may go.
        e.add("mine.exe").unwrap();
        e.remove(last, true).unwrap();
        assert_eq!(e.list().len(), 1);
        // ... and now the user entry is the last one standing.
        assert_eq!(
            e.remove("mine.exe", false),
            Err(DenyEditError::WouldBeEmpty)
        );
        assert!(!e.list().is_empty());
    }

    #[test]
    fn reset_needs_confirm_restores_defaults_and_keeps_user_entries() {
        let (d, mut e) = editor();
        e.add("mine.exe").unwrap();
        e.remove("cmd.exe", true).unwrap();
        e.remove("pwsh.exe", true).unwrap();
        assert_eq!(e.reset_to_defaults(false), Err(DenyEditError::NeedsConfirm));
        assert_eq!(e.removed_defaults().len(), 2);
        e.reset_to_defaults(true).unwrap();
        assert!(e.removed_defaults().is_empty());
        assert!(e.list().matches("cmd.exe") && e.list().matches("mine.exe"));
        assert_eq!(file(&d).remove, Vec::<String>::new());
        assert_eq!(file(&d).add, vec!["mine.exe".to_string()]);
    }

    #[test]
    fn broken_files_fall_back_to_defaults_with_an_issue_until_the_first_change() {
        for (text, issue) in [
            ("not json", "invalid"),
            (r#"{"add": 5}"#, "invalid"),
            (r#"{"disable": true}"#, "invalid"),
        ] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(config_path(d.path()), text).unwrap();
            let mut e = DenyEditor::load(d.path().to_path_buf());
            assert_eq!(e.load_issue(), Some(issue), "{text}");
            assert_eq!(*e.list(), DenyList::defaults());
            assert_eq!(e.view("").load_issue.as_deref(), Some(issue));
            e.add("mine.exe").unwrap();
            assert!(e.load_issue().is_none());
            assert!(DenyEditor::load(d.path().to_path_buf())
                .list()
                .matches("mine.exe"));
        }
        // A directory where the file should be.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(config_path(d.path())).unwrap();
        let e = DenyEditor::load(d.path().to_path_buf());
        assert_eq!(e.load_issue(), Some("unreadable"));
        assert!(!e.list().is_empty());
        // A file that removes everything.
        let d = tempfile::tempdir().unwrap();
        let all: Vec<String> = DenyList::defaults().entries().map(String::from).collect();
        std::fs::write(
            config_path(d.path()),
            serde_json::to_vec(&UserConfig {
                add: vec![],
                remove: all,
            })
            .unwrap(),
        )
        .unwrap();
        let e = DenyEditor::load(d.path().to_path_buf());
        assert_eq!(e.load_issue(), Some("would_be_empty"));
        assert_eq!(*e.list(), DenyList::defaults());
        assert!(e.removed_defaults().is_empty());
    }

    #[test]
    fn save_failure_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let blocker = d.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let mut e = DenyEditor::load(blocker.join("cfg"));
        assert_eq!(e.add("mine.exe"), Err(DenyEditError::Io));
        assert!(!e.list().matches("mine.exe"));
    }

    #[test]
    fn error_tokens_are_fixed() {
        assert_eq!(DenyEditError::NeedsConfirm.to_string(), "needs_confirm");
        assert_eq!(DenyEditError::WouldBeEmpty.to_string(), "would_be_empty");
    }
}
