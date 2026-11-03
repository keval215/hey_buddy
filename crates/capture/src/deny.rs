//! App deny-list: defaults + user additions from a config file.
//!
//! Matching is by app identifier (bundle id on macOS, exe file name on Windows), ASCII
//! case-insensitive, exact. The list returned by loading is never empty: defaults can only be
//! removed through an explicit `remove` entry (the removed ids are reported in
//! `LoadedDenyList::removed_defaults`), a config that would leave the list empty falls back to
//! the full defaults with `DenyListError::WouldBeEmpty`, and a corrupt or unreadable config
//! falls back to the full defaults with the error surfaced.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Config file name inside the config dir.
pub const CONFIG_FILE: &str = "deny_list.json";

/// Default deny-listed app identifiers (bundle ids and Windows exe names, one namespace).
///
/// Entries marked `(unverified)` are best-effort guesses; the user can correct them in the
/// config file.
///
/// Banking apps are INTENTIONALLY not listed: bank bundle ids/exe names are regional and we
/// will not invent them. Users add their own via `add`; core/UI should tell the user this.
const DEFAULTS: &[&str] = &[
    // Password managers
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.agilebits.onepassword-osx",
    "1password.exe",
    "com.bitwarden.desktop",
    "bitwarden.exe",
    "org.keepassxc.keepassxc",
    "keepassxc.exe",
    "keepass.exe",
    "com.callpod.keepersecurity",   // (unverified) Keeper
    "keeperpasswordmanager.exe",    // (unverified) Keeper
    "nordpass.exe",                 // (unverified)
    "com.nordsec.nordpass",         // (unverified)
    "enpass.exe",                   // (unverified)
    "in.sinew.enpass-desktop",      // (unverified) Enpass
    "roboform.exe",                 // (unverified)
    "com.bitwarden.desktop.safari", // (unverified)
    "com.lastpass.lastpass",        // (unverified)
    "lastpass.exe",                 // (unverified)
    "com.dashlane.dashlanemac",     // (unverified)
    "dashlane.exe",
    "com.apple.keychainaccess",
    "com.apple.passwords", // (unverified)
    // Terminals
    "com.apple.terminal",
    "com.googlecode.iterm2",
    "dev.warp.warp-stable",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "org.alacritty", // (unverified)
    "windowsterminal.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "conhost.exe",
    "openconsole.exe",
    // Private messengers
    "org.whispersystems.signal-desktop",
    "signal.exe",
    "net.whatsapp.whatsapp",
    "whatsapp.exe",
    "ru.keepcoder.telegram",
    "org.telegram.desktop",
    "telegram.exe",
    "com.apple.mobilesms", // Messages
];

fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// User config file shape.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Extra app identifiers to deny.
    #[serde(default)]
    pub add: Vec<String>,
    /// Default entries to explicitly remove. Only exact default ids have any effect.
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DenyListError {
    #[error("deny-list config could not be read; using built-in defaults")]
    Unreadable,
    #[error("deny-list config is invalid; using built-in defaults")]
    Invalid,
    #[error("deny-list config would leave the list empty; using built-in defaults")]
    WouldBeEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyList {
    entries: BTreeSet<String>,
}

/// Result of loading: the list is ALWAYS usable (never empty). `error` and `removed_defaults`
/// must be surfaced to the user by core/UI.
#[derive(Debug)]
pub struct LoadedDenyList {
    pub list: DenyList,
    pub error: Option<DenyListError>,
    /// Default ids the user explicitly removed (normalised). Empty if none or on fallback.
    pub removed_defaults: Vec<String>,
}

impl LoadedDenyList {
    fn defaults(error: Option<DenyListError>) -> Self {
        Self {
            list: DenyList::defaults(),
            error,
            removed_defaults: vec![],
        }
    }
}

impl DenyList {
    pub fn defaults() -> Self {
        Self {
            entries: DEFAULTS.iter().map(|s| norm(s)).collect(),
        }
    }

    /// Defaults, minus explicit removals, plus additions (an id both added and removed is kept).
    /// Never empty: see [`DenyList::apply_config`].
    pub fn from_config(cfg: &UserConfig) -> Self {
        Self::apply_config(cfg).list
    }

    /// Like [`DenyList::from_config`] but reports removed defaults and the empty-list fallback.
    pub fn apply_config(cfg: &UserConfig) -> LoadedDenyList {
        let mut entries = Self::defaults().entries;
        let mut removed = BTreeSet::new();
        for r in &cfg.remove {
            let r = norm(r);
            if entries.remove(&r) {
                removed.insert(r);
            }
        }
        for a in &cfg.add {
            let a = norm(a);
            if !a.is_empty() {
                // Added after removal: an id both added and removed stays denied.
                removed.remove(&a);
                entries.insert(a);
            }
        }
        if entries.is_empty() {
            return LoadedDenyList::defaults(Some(DenyListError::WouldBeEmpty));
        }
        LoadedDenyList {
            list: Self { entries },
            error: None,
            removed_defaults: removed.into_iter().collect(),
        }
    }

    pub fn matches(&self, app_id: &str) -> bool {
        self.entries.contains(&norm(app_id))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }

    /// Load from `<config_dir>/deny_list.json`. Missing file = defaults, no error.
    /// Unreadable/corrupt file = defaults plus an error. Never an empty list.
    pub fn load(config_dir: &Path) -> LoadedDenyList {
        let path = config_path(config_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return LoadedDenyList::defaults(None);
            }
            Err(_) => return LoadedDenyList::defaults(Some(DenyListError::Unreadable)),
        };
        match serde_json::from_str::<UserConfig>(&text) {
            Ok(cfg) => Self::apply_config(&cfg),
            Err(_) => LoadedDenyList::defaults(Some(DenyListError::Invalid)),
        }
    }
}

/// Where the config file lives, given the app's config directory.
pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) {
        std::fs::write(config_path(dir), text).unwrap();
    }

    #[test]
    fn defaults_cover_categories_case_insensitively() {
        let d = DenyList::defaults();
        for id in [
            "com.1Password.1password",
            "KeePassXC.exe",
            "Bitwarden.exe",
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "WindowsTerminal.exe",
            "org.whispersystems.signal-desktop",
            "net.whatsapp.WhatsApp",
            "ru.keepcoder.Telegram",
            "com.apple.MobileSMS",
        ] {
            assert!(d.matches(id), "{id}");
        }
        assert!(!d.matches("com.apple.finder"));
        assert!(!d.matches(""));
    }

    #[test]
    fn missing_file_gives_defaults_no_error() {
        let t = tempfile::tempdir().unwrap();
        let l = DenyList::load(t.path());
        assert!(l.error.is_none());
        assert_eq!(l.list, DenyList::defaults());
    }

    #[test]
    fn merge_add_and_explicit_remove() {
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            r#"{"add":["  Com.Bank.App "],"remove":["cmd.exe"]}"#,
        );
        let l = DenyList::load(t.path());
        assert!(l.error.is_none());
        assert!(l.list.matches("com.bank.app"));
        assert!(!l.list.matches("cmd.exe"));
        assert!(l.list.matches("powershell.exe"));
        assert_eq!(l.list.len(), DenyList::defaults().len());
        assert_eq!(l.removed_defaults, vec!["cmd.exe".to_string()]);
    }

    #[test]
    fn removing_everything_without_additions_falls_back_to_defaults() {
        let t = tempfile::tempdir().unwrap();
        let all: Vec<String> = DenyList::defaults().entries().map(String::from).collect();
        let text = serde_json::to_string(&UserConfig {
            add: vec![],
            remove: all,
        })
        .unwrap();
        write(t.path(), &text);
        let l = DenyList::load(t.path());
        assert!(matches!(l.error, Some(DenyListError::WouldBeEmpty)));
        assert_eq!(l.list, DenyList::defaults());
        assert!(!l.list.is_empty());
        assert!(l.removed_defaults.is_empty());
    }

    #[test]
    fn removing_all_defaults_but_adding_one_is_allowed_and_reported() {
        let all: Vec<String> = DenyList::defaults().entries().map(String::from).collect();
        let n = all.len();
        let l = DenyList::apply_config(&UserConfig {
            add: vec!["My.Bank".into()],
            remove: all,
        });
        assert!(l.error.is_none());
        assert_eq!(l.list.len(), 1);
        assert!(l.list.matches("my.bank"));
        assert_eq!(l.removed_defaults.len(), n);
    }

    #[test]
    fn readding_a_removed_default_is_not_reported_removed() {
        let l = DenyList::apply_config(&UserConfig {
            add: vec!["CMD.exe".into()],
            remove: vec!["cmd.exe".into()],
        });
        assert!(l.list.matches("cmd.exe"));
        assert!(l.removed_defaults.is_empty());
    }

    #[test]
    fn keepass_is_default() {
        assert!(DenyList::defaults().matches("KeePass.exe"));
    }

    #[test]
    fn empty_config_cannot_disable_list() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "{}");
        assert_eq!(DenyList::load(t.path()).list, DenyList::defaults());
        // A blank/wildcard add or unknown remove removes nothing.
        let cfg = UserConfig {
            add: vec!["".into()],
            remove: vec!["*".into(), "nope".into()],
        };
        assert_eq!(DenyList::from_config(&cfg), DenyList::defaults());
    }

    #[test]
    fn corrupt_config_fails_closed() {
        for text in ["not json", r#"{"add": 5}"#, r#"{"disable": true}"#, ""] {
            let t = tempfile::tempdir().unwrap();
            write(t.path(), text);
            let l = DenyList::load(t.path());
            assert!(matches!(l.error, Some(DenyListError::Invalid)), "{text}");
            assert_eq!(l.list, DenyList::defaults());
            assert!(!l.list.is_empty());
        }
    }

    #[test]
    fn unreadable_config_fails_closed() {
        let t = tempfile::tempdir().unwrap();
        // A directory where the file should be: read fails with a non-NotFound error.
        std::fs::create_dir(config_path(t.path())).unwrap();
        let l = DenyList::load(t.path());
        assert!(matches!(l.error, Some(DenyListError::Unreadable)));
        assert_eq!(l.list, DenyList::defaults());
    }

    #[test]
    fn error_messages_have_no_path_or_content() {
        let m = DenyListError::Invalid.to_string();
        assert!(!m.contains('/'));
    }
}
