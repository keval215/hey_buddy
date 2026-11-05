//! Apps no action may target (ADR 0003 section 5 rows 19 and 24). Names are lowercase exe stems;
//! matching strips `.exe` / `.msc` and compares whole names, plus the installer heuristics of
//! ADR 0004 section 8 (`setup*`, `install*`, `*-installer`).

use firewall::AppId;

const BUILTIN: &[&str] = &[
    "cmd",
    "powershell",
    "powershell_ise",
    "pwsh",
    "wt",
    "windowsterminal",
    "openconsole",
    "conhost",
    "wsl",
    "bash",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "systemsettings",
    "control",
    "mmc",
    "regedit",
    "regedt32",
    "gpedit",
    "services",
    "taskmgr",
    "msiexec",
    "winget",
    "store",
    "winstore.app",
    "windowsstore",
    "microsoftstore",
    "microsoft.windowsstore",
    // Auth surfaces (also invisible to observe; belt and braces).
    "consent",
    "credentialuibroker",
    "logonui",
    "lockapp",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeverApps {
    names: Vec<String>,
}

fn stem(app: &str) -> String {
    let a = app.trim().to_lowercase();
    for suf in [".exe", ".msc"] {
        if let Some(s) = a.strip_suffix(suf) {
            return s.to_owned();
        }
    }
    a
}

impl NeverApps {
    /// The built-in list. Callers may add names but there is no way to remove one.
    pub fn builtin() -> Self {
        Self {
            names: BUILTIN.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Adds a name (stricter only).
    pub fn with_extra(mut self, name: &str) -> Self {
        let s = stem(name);
        if !s.is_empty() && !self.names.contains(&s) {
            self.names.push(s);
        }
        self
    }

    pub fn contains(&self, app: &AppId) -> bool {
        let s = stem(app.as_str());
        self.names.contains(&s)
            || s.starts_with("setup")
            || s.starts_with("install")
            || s.ends_with("-installer")
    }
}

impl Default for NeverApps {
    fn default() -> Self {
        Self::builtin()
    }
}

/// File managers, where Delete-type chords delete files (row 19): explorer and the common
/// third-party managers.
pub(crate) fn is_file_manager(app: &AppId) -> bool {
    matches!(
        stem(app.as_str()).as_str(),
        "explorer"
            | "files"
            | "totalcmd"
            | "totalcmd64"
            | "doublecmd"
            | "freecommander"
            | "xyplorer"
            | "q-dir"
            | "onecommander"
            | "directoryopus"
            | "dopus"
            | "explorer++"
            | "multicommander"
            | "finder"
    )
}
