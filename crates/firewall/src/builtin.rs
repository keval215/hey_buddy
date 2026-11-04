//! Built-in app lists (ADR 0004 §8). Not user-removable: they live in the firewall, not in the
//! user's deny-list config.
//!
//! - **Auth surfaces** (credential / UAC / lock prompts) are invisible: added to the deny policy
//!   used by `observe`, refused again at `revalidate`.
//! - **Restricted apps** are observable but never a target (no actions in v1).

use frame::AppDenyPolicy;

/// Windows exe names of credential, consent and lock surfaces (VERIFY on Windows, ADR 0004 §19).
const AUTH_SURFACES: &[&str] = &[
    "consent.exe",
    "credentialuibroker.exe",
    "logonui.exe",
    "lockapp.exe",
];

const RESTRICTED: &[&str] = &[
    "explorer.exe",
    "systemsettings.exe",
    "control.exe",
    "regedit.exe",
    "mmc.exe",
    "taskmgr.exe",
    "msiexec.exe",
    "mstsc.exe",
];

pub(crate) fn is_auth_surface(app_id: &str) -> bool {
    let a = app_id.trim().to_ascii_lowercase();
    AUTH_SURFACES.contains(&a.as_str())
}

pub(crate) fn is_restricted(app_id: &str) -> bool {
    let a = app_id.trim().to_ascii_lowercase();
    RESTRICTED.contains(&a.as_str())
        || a.starts_with("setup")
        || a.starts_with("install")
        || a.ends_with("-installer.exe")
}

/// The user's deny policy plus the built-in auth surfaces.
pub(crate) struct WithAuthSurfaces<'a>(pub(crate) &'a dyn AppDenyPolicy);

impl AppDenyPolicy for WithAuthSurfaces<'_> {
    fn is_denied(&self, app_id: &str) -> bool {
        is_auth_surface(app_id) || self.0.is_denied(app_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_surfaces() {
        for a in [
            "consent.exe",
            "CredentialUIBroker.exe",
            "logonui.exe",
            "LockApp.exe",
        ] {
            assert!(is_auth_surface(a), "{a}");
        }
        assert!(!is_auth_surface("notepad.exe"));
        assert!(!is_auth_surface("consent"));
    }

    #[test]
    fn restricted_apps() {
        for a in [
            "explorer.exe",
            "SystemSettings.exe",
            "control.exe",
            "regedit.exe",
            "mmc.exe",
            "taskmgr.exe",
            "msiexec.exe",
            "mstsc.exe",
            "setup.exe",
            "setup_x64.exe",
            "installer.exe",
            "install-foo.exe",
            "foo-installer.exe",
        ] {
            assert!(is_restricted(a), "{a}");
        }
        for a in ["notepad.exe", "chrome.exe", "myexplorer.exe", "xsetup.exe"] {
            assert!(!is_restricted(a), "{a}");
        }
    }
}
