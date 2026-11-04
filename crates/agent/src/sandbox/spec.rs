//! Pure decisions of the Windows launch (ADR 0003 section 3), compiled and unit-tested on every
//! platform: path validation, command line, environment block, capability list, job limits.
//! `windows.rs` only feeds these into the Win32 calls (and asserts at compile time that the
//! constants here equal the `windows` crate's).

/// AppContainer profile name.
pub const APPCONTAINER_NAME: &str = "hey_buddy.agent";
pub const APPCONTAINER_DISPLAY: &str = "hey_buddy agent engine";
/// ZERO capabilities: no network (incl. loopback), no user files, no devices, no clipboard.
pub const CAPABILITIES: &[&str] = &[];
/// The only executable name that may be launched.
pub const ENGINE_EXE_NAME: &str = "buddy-agent.exe";

pub const JOB_MEMORY_LIMIT_BYTES: usize = 512 * 1024 * 1024;
pub const JOB_ACTIVE_PROCESS_LIMIT: u32 = 1;
// JOB_OBJECT_LIMIT_*
pub const LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
pub const LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
pub const LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
pub const LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
pub const JOB_LIMIT_FLAGS: u32 = LIMIT_ACTIVE_PROCESS
    | LIMIT_PROCESS_MEMORY
    | LIMIT_DIE_ON_UNHANDLED_EXCEPTION
    | LIMIT_KILL_ON_JOB_CLOSE;
// JOB_OBJECT_UILIMIT_*: HANDLES | READCLIPBOARD | WRITECLIPBOARD | SYSTEMPARAMETERS |
// DISPLAYSETTINGS | GLOBALATOMS | DESKTOP | EXITWINDOWS
pub const UILIMIT_HANDLES: u32 = 0x01;
pub const UILIMIT_READCLIPBOARD: u32 = 0x02;
pub const UILIMIT_WRITECLIPBOARD: u32 = 0x04;
pub const UILIMIT_SYSTEMPARAMETERS: u32 = 0x08;
pub const UILIMIT_DISPLAYSETTINGS: u32 = 0x10;
pub const UILIMIT_GLOBALATOMS: u32 = 0x20;
pub const UILIMIT_DESKTOP: u32 = 0x40;
pub const UILIMIT_EXITWINDOWS: u32 = 0x80;
pub const JOB_UI_LIMITS: u32 = UILIMIT_HANDLES
    | UILIMIT_READCLIPBOARD
    | UILIMIT_WRITECLIPBOARD
    | UILIMIT_SYSTEMPARAMETERS
    | UILIMIT_DISPLAYSETTINGS
    | UILIMIT_GLOBALATOMS
    | UILIMIT_DESKTOP
    | UILIMIT_EXITWINDOWS;

/// `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED`: the engine may not create child processes.
pub const CHILD_PROCESS_RESTRICTED: u32 = 0x01;
/// Mitigation policy bits (`PROCESS_CREATION_MITIGATION_POLICY_*_ALWAYS_ON`, first DWORD64):
/// strict handle checks (1<<24), no remote image loads (1<<52), no low-label image loads (1<<56).
/// VERIFY on the target Windows build; a refusal fails the whole spawn (no fallback).
pub const MITIGATION_STRICT_HANDLE_CHECKS: u64 = 1 << 24;
pub const MITIGATION_IMAGE_LOAD_NO_REMOTE: u64 = 1 << 52;
pub const MITIGATION_IMAGE_LOAD_NO_LOW_LABEL: u64 = 1 << 56;
pub const MITIGATION_POLICY: u64 = MITIGATION_STRICT_HANDLE_CHECKS
    | MITIGATION_IMAGE_LOAD_NO_REMOTE
    | MITIGATION_IMAGE_LOAD_NO_LOW_LABEL;

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

/// A local absolute drive path with backslashes only: no UNC (`\\server`), no device or verbatim
/// prefix (`\\?\`, `\\.\`), no `/`, no `..` or `.` components, no empty components, no control
/// characters, quotes or wildcard characters.
pub fn validate_local_path(p: &str) -> Result<(), &'static str> {
    if p.len() > 260 || !is_drive_path(p) {
        return Err("not_absolute_local");
    }
    if p.chars()
        .any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | '*' | '?' | '/'))
    {
        return Err("bad_char");
    }
    // After "X:\": components separated by single backslashes.
    for comp in p[3..].split('\\') {
        if comp.is_empty() && p.len() > 3 && !p.ends_with('\\') {
            return Err("empty_component");
        }
        if comp == "." || comp == ".." {
            return Err("dot_component");
        }
        if comp.ends_with('.') || comp.ends_with(' ') {
            return Err("bad_component");
        }
        // The ':' of an alternate data stream.
        if comp.contains(':') {
            return Err("bad_component");
        }
    }
    Ok(())
}

/// The engine binary must be `<install_dir>\buddy-agent.exe`, nothing else.
pub fn validate_exe_path(exe: &str, install_dir: &str) -> Result<(), &'static str> {
    validate_local_path(exe)?;
    validate_local_path(install_dir)?;
    let dir = install_dir.trim_end_matches('\\');
    let expected = format!("{dir}\\{ENGINE_EXE_NAME}");
    if exe.eq_ignore_ascii_case(&expected) {
        Ok(())
    } else {
        Err("not_in_install_dir")
    }
}

/// `"<exe>"` and no arguments. Refuses a path containing a quote.
pub fn quote_command_line(exe: &str) -> Option<String> {
    if exe.contains('"') || exe.contains('\0') || exe.is_empty() {
        return None;
    }
    Some(format!("\"{exe}\""))
}

/// The explicit, minimal environment block (UTF-16, `KEY=VALUE\0` entries sorted by name, then a
/// final `\0`). Only `SystemRoot`, `TEMP` and `TMP`: nothing from the user's environment (it may
/// hold API keys).
pub fn build_env_block(system_root: &str, container_folder: &str) -> Option<Vec<u16>> {
    let bad = |s: &str| s.is_empty() || s.contains('\0') || s.contains('\n') || s.contains('=');
    if bad(system_root) || bad(container_folder) {
        return None;
    }
    let mut entries = [
        ("SystemRoot", system_root),
        ("TEMP", container_folder),
        ("TMP", container_folder),
    ];
    entries.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut out: Vec<u16> = Vec::new();
    for (k, v) in entries {
        out.extend(format!("{k}={v}").encode_utf16());
        out.push(0);
    }
    out.push(0);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "C:\\Program Files\\hey_buddy";
    const EXE: &str = "C:\\Program Files\\hey_buddy\\buddy-agent.exe";

    #[test]
    fn accepts_the_install_dir_binary() {
        assert_eq!(validate_exe_path(EXE, DIR), Ok(()));
        assert_eq!(
            validate_exe_path(&EXE.to_lowercase(), &DIR.to_uppercase()),
            Ok(())
        );
        assert_eq!(validate_exe_path(EXE, &format!("{DIR}\\")), Ok(()));
    }

    #[test]
    fn rejects_everything_else() {
        let bad = [
            "buddy-agent.exe",
            "\\\\server\\share\\buddy-agent.exe",
            "\\\\?\\C:\\Program Files\\hey_buddy\\buddy-agent.exe",
            "\\\\.\\C:\\x\\buddy-agent.exe",
            "C:/Program Files/hey_buddy/buddy-agent.exe",
            "C:\\Program Files\\hey_buddy\\..\\evil\\buddy-agent.exe",
            "C:\\Program Files\\hey_buddy\\.\\buddy-agent.exe",
            "C:\\Program Files\\hey_buddy\\buddy-agent.exe:stream",
            "C:\\Program Files\\hey_buddy\\other.exe",
            "C:\\Program Files\\hey_buddy\\sub\\buddy-agent.exe",
            "C:\\Program Files\\hey_buddy2\\buddy-agent.exe",
            "C:\\Users\\x\\buddy-agent.exe",
            "C:\\Program Files\\hey_buddy\\\"x.exe",
            "C:\\Program Files\\hey_buddy\\buddy-agent.exe\0",
            "C:\\Program Files\\hey_buddy\\buddy-agent.exe ",
            "",
            "C:",
            "C:buddy-agent.exe",
        ];
        for b in bad {
            assert!(validate_exe_path(b, DIR).is_err(), "accepted {b:?}");
        }
    }

    #[test]
    fn local_path_rules() {
        assert!(validate_local_path("C:\\a\\b").is_ok());
        assert!(validate_local_path("C:\\a\\\\b").is_err());
        assert!(validate_local_path("C:\\a\\..\\b").is_err());
        assert!(validate_local_path("C:\\a*").is_err());
        assert!(validate_local_path(&format!("C:\\{}", "a".repeat(300))).is_err());
    }

    #[test]
    fn command_line_is_quoted_and_has_no_arguments() {
        assert_eq!(
            quote_command_line(EXE).as_deref(),
            Some("\"C:\\Program Files\\hey_buddy\\buddy-agent.exe\"")
        );
        assert_eq!(quote_command_line("a\" b"), None);
        assert_eq!(quote_command_line(""), None);
    }

    #[test]
    fn env_block_is_minimal_sorted_and_double_nul_terminated() {
        let b = build_env_block("C:\\Windows", "C:\\c\\f").unwrap();
        let s = String::from_utf16(&b).unwrap();
        assert_eq!(s, "SystemRoot=C:\\Windows\0TEMP=C:\\c\\f\0TMP=C:\\c\\f\0\0");
        assert_eq!(&b[b.len() - 2..], &[0, 0]);
        // Nothing but the three variables: no PATH, no USERPROFILE, no keys.
        assert_eq!(s.matches('=').count(), 3);
        assert!(build_env_block("", "x").is_none());
        assert!(build_env_block("C:\\Windows\0", "x").is_none());
        assert!(build_env_block("C:\\W", "a=b").is_none());
    }

    #[test]
    fn zero_capabilities_and_job_limits() {
        assert!(CAPABILITIES.is_empty());
        assert_eq!(APPCONTAINER_NAME, "hey_buddy.agent");
        assert_eq!(JOB_ACTIVE_PROCESS_LIMIT, 1);
        assert_eq!(JOB_MEMORY_LIMIT_BYTES, 536_870_912);
        assert_eq!(JOB_UI_LIMITS, 0xFF);
        assert_eq!(JOB_LIMIT_FLAGS, 0x2000 | 0x400 | 0x100 | 0x8);
        assert_eq!(CHILD_PROCESS_RESTRICTED, 1);
        assert_ne!(MITIGATION_POLICY & MITIGATION_IMAGE_LOAD_NO_REMOTE, 0);
    }
}
