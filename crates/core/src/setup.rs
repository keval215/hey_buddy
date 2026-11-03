//! Startup helpers for the app shell, so `apps/desktop` needs no direct dependency on the leaf
//! crates (ADR 0001): the OS firewall and the app deny list with its user-facing notes.

use std::path::Path;
use std::sync::Arc;

use capture::DenyList;
use firewall::Firewall;

/// The firewall for this OS. Fails closed (everything `Blocked`) on platforms without a
/// secure-field detector.
pub fn platform_firewall() -> Arc<dyn Firewall> {
    Arc::from(firewall::platform_firewall())
}

/// The deny list plus the notes the UI must show (`Status.notices`).
pub struct DenyListSetup {
    pub list: Arc<DenyList>,
    pub notices: Vec<String>,
}

/// Always shown: bank bundle ids / exe names are regional, so none are on the default list.
pub const BANKING_NOTICE: &str = "Banking apps are not on the default deny list. Add yours by hand in deny_list.json in the config folder (\"add\": [\"app id\"]).";

/// Load `<config_dir>/deny_list.json` (defaults when missing). Problems are never silent: an
/// unreadable or invalid file falls back to the defaults and says so, and defaults the user
/// removed are listed.
pub fn load_deny_list(config_dir: &Path) -> DenyListSetup {
    let loaded = DenyList::load(config_dir);
    let mut notices = vec![BANKING_NOTICE.to_string()];
    if let Some(e) = &loaded.error {
        notices.push(format!("Deny list: {e}."));
    }
    if !loaded.removed_defaults.is_empty() {
        notices.push(format!(
            "You removed these apps from the default deny list: {}.",
            loaded.removed_defaults.join(", ")
        ));
    }
    DenyListSetup {
        list: Arc::new(loaded.list),
        notices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_only_the_banking_note() {
        let d = tempfile::tempdir().unwrap();
        let s = load_deny_list(d.path());
        assert!(!s.list.is_empty());
        assert_eq!(s.notices, vec![BANKING_NOTICE.to_string()]);
    }

    #[test]
    fn corrupt_file_and_removed_defaults_are_surfaced() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(capture::deny::config_path(d.path()), b"{not json").unwrap();
        let s = load_deny_list(d.path());
        assert!(
            s.list.matches("com.1password.1password"),
            "defaults are back"
        );
        assert!(
            s.notices.iter().any(|n| n.contains("invalid")),
            "{:?}",
            s.notices
        );

        std::fs::write(
            capture::deny::config_path(d.path()),
            br#"{"remove":["cmd.exe"],"add":["bank.exe"]}"#,
        )
        .unwrap();
        let s = load_deny_list(d.path());
        assert!(s.list.matches("bank.exe") && !s.list.matches("cmd.exe"));
        assert!(
            s.notices.iter().any(|n| n.contains("cmd.exe")),
            "{:?}",
            s.notices
        );
    }

    #[test]
    fn platform_firewall_is_usable() {
        // On Linux this is the fail-closed unsupported firewall.
        let fw = platform_firewall();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert!(matches!(fw.preflight(), firewall::SessionState::Blocked(_)));
        let _ = fw;
    }
}
