//! Validation of the user-chosen models directory (S2-7). The check is lexical first (so the
//! rules are the same on every OS and testable on Linux) and then looks at the filesystem
//! without creating anything. Failures are fixed [`IssueCode`]s: the rejected text is never
//! echoed.

use std::fs;
use std::path::Path;

use crate::api::IssueCode;

/// Longest accepted path (bytes).
pub const MAX_PATH_LEN: usize = 1024;

/// Characters that hide or reorder text: control, bidirectional marks and overrides,
/// zero-width characters, byte order mark, line and paragraph separators.
pub(crate) fn is_hostile_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Windows device names (with or without an extension they still address the device).
fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_lowercase();
    if matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$" | "clock$"
    ) {
        return true;
    }
    let b = stem.as_bytes();
    b.len() == 4
        && (stem.starts_with("com") || stem.starts_with("lpt"))
        && matches!(b[3], b'1'..=b'9')
}

/// Unix top-level directories that are never a models folder, with everything below them.
const UNIX_SUBTREES: &[&str] = &[
    "etc",
    "bin",
    "sbin",
    "usr",
    "lib",
    "lib32",
    "lib64",
    "libx32",
    "boot",
    "dev",
    "proc",
    "sys",
    "system",
    "library",
    "private/etc",
];
/// Unix directories that are refused themselves (but may contain a models folder).
const UNIX_EXACT: &[&str] = &[
    "var",
    "opt",
    "tmp",
    "root",
    "run",
    "srv",
    "mnt",
    "media",
    "snap",
    "home",
    "users",
    "volumes",
    "applications",
    "private",
    "private/var",
    "private/tmp",
];
/// Directories whose direct children are home directories (`/home/me`): refused too.
const HOME_PARENTS: &[&str] = &["home", "users", "volumes", "mnt", "media"];
/// Windows system directories (after the drive), with everything below them.
const WIN_SUBTREES: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "system volume information",
    "$recycle.bin",
    "recovery",
];
const WIN_EXACT: &[&str] = &["users", "programdata", "documents and settings"];

/// Inside a system directory tree (or the tree root's children that are never ours).
/// `components` are lowercase and without the root or drive.
fn in_system_subtree(components: &[String], windows: bool) -> bool {
    let joined = components.join("/");
    let under = |names: &[&str]| {
        names
            .iter()
            .any(|n| joined == *n || joined.starts_with(&format!("{n}/")))
    };
    if windows {
        under(WIN_SUBTREES)
    } else {
        under(UNIX_SUBTREES)
    }
}

/// A system directory itself, a root, or a home directory: refused as the folder to use.
fn is_system_dir(components: &[String], windows: bool) -> bool {
    if components.is_empty() || in_system_subtree(components, windows) {
        return true;
    }
    let joined = components.join("/");
    if windows {
        WIN_EXACT.contains(&joined.as_str()) || (components.len() == 2 && components[0] == "users")
    } else {
        UNIX_EXACT.contains(&joined.as_str())
            || (components.len() == 2 && HOME_PARENTS.contains(&components[0].as_str()))
    }
}

/// Lexical check. Returns the cleaned path (trimmed, no trailing separators) or the reason.
/// An empty input is valid and means "use the default".
pub fn check_lexical(input: &str) -> Result<String, IssueCode> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    if s.len() > MAX_PATH_LEN {
        return Err(IssueCode::TooLong);
    }
    if s.chars().any(is_hostile_char) {
        return Err(IssueCode::ControlChars);
    }
    if s.starts_with("\\\\") || s.starts_with("//") {
        return Err(IssueCode::NetworkPath);
    }
    if s.chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(IssueCode::InvalidFormat);
    }
    let norm = s.replace('\\', "/");
    let b = norm.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let rest = if drive {
        if b.len() < 3 || b[2] != b'/' {
            return Err(IssueCode::NotAbsolute); // `C:` or `C:folder` (drive-relative)
        }
        &norm[3..]
    } else if let Some(r) = norm.strip_prefix('/') {
        r
    } else {
        return Err(IssueCode::NotAbsolute);
    };
    if rest.contains(':') {
        return Err(IssueCode::InvalidFormat); // alternate data streams, odd syntax
    }
    let mut components = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err(IssueCode::Traversal),
            c => {
                if c.ends_with('.') || c.ends_with(' ') {
                    return Err(IssueCode::InvalidFormat);
                }
                if is_device_name(c) {
                    return Err(IssueCode::DeviceName);
                }
                components.push(c.to_lowercase());
            }
        }
    }
    if is_system_dir(&components, drive) {
        return Err(IssueCode::SystemDir);
    }
    Ok(s.trim_end_matches(['/', '\\']).to_string())
}

/// Filesystem check, creating nothing: an existing path must be a real directory (not a
/// symbolic link); a missing one must hang off an existing directory so it can be created
/// later. The resolved location must not be a system directory either.
pub fn check_filesystem(path: &str) -> Result<(), IssueCode> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(IssueCode::NotAbsolute);
    }
    let mut exists = true;
    let nearest = match fs::symlink_metadata(p) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(IssueCode::Symlink);
            }
            if !md.is_dir() {
                return Err(IssueCode::NotADirectory);
            }
            p
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            exists = false;
            let mut found = None;
            for a in p.ancestors().skip(1) {
                match fs::metadata(a) {
                    Ok(md) if md.is_dir() => {
                        found = Some(a);
                        break;
                    }
                    Ok(_) => return Err(IssueCode::NotCreatable),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(IssueCode::NotCreatable),
                }
            }
            found.ok_or(IssueCode::NotCreatable)?
        }
        Err(_) => return Err(IssueCode::NotCreatable),
    };
    // A symlinked parent can point into a system directory: check where it really is.
    if let Ok(real) = fs::canonicalize(nearest) {
        let text = real.to_string_lossy().replace('\\', "/");
        let text = text.strip_prefix("//?/").unwrap_or(&text);
        let (windows, rest) = match text.as_bytes() {
            [d, b':', b'/', ..] if d.is_ascii_alphabetic() => (true, &text[3..]),
            _ => (false, text.trim_start_matches('/')),
        };
        let comps: Vec<String> = rest
            .split('/')
            .filter(|c| !c.is_empty())
            .map(str::to_lowercase)
            .collect();
        // A system tree is refused wherever it is reached from. The directory itself (or a
        // root) is refused only when it IS the chosen folder: `/tmp/models` may hang off
        // `/tmp`.
        if in_system_subtree(&comps, windows) || (exists && is_system_dir(&comps, windows)) {
            return Err(IssueCode::SystemDir);
        }
    }
    Ok(())
}

/// Full validation. `Ok("")` = use the default; otherwise the cleaned absolute path.
pub fn validate_models_dir(input: &str) -> Result<String, IssueCode> {
    let clean = check_lexical(input)?;
    if clean.is_empty() {
        return Ok(clean);
    }
    check_filesystem(&clean)?;
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_table() {
        let long = format!("/tmp/{}", "a".repeat(10_240));
        let cases: Vec<(String, Result<(), IssueCode>)> = vec![
            ("".into(), Ok(())),
            ("   ".into(), Ok(())),
            ("/tmp/x/models".into(), Ok(())),
            ("/tmp/x/models/".into(), Ok(())),
            ("/data/hey buddy/models".into(), Ok(())),
            ("/data/mod\u{e9}ls".into(), Ok(())),
            ("C:\\Data\\models".into(), Ok(())),
            ("c:/data/models".into(), Ok(())),
            // boundary: exactly the maximum is fine, one more is not
            (format!("/{}", "a".repeat(MAX_PATH_LEN - 1)), Ok(())),
            (
                format!("/{}", "a".repeat(MAX_PATH_LEN)),
                Err(IssueCode::TooLong),
            ),
            (long, Err(IssueCode::TooLong)),
            // traversal
            ("/tmp/../etc".into(), Err(IssueCode::Traversal)),
            ("/tmp/x/..".into(), Err(IssueCode::Traversal)),
            ("C:\\data\\..\\windows".into(), Err(IssueCode::Traversal)),
            ("..".into(), Err(IssueCode::NotAbsolute)),
            // relative and drive-relative
            ("models".into(), Err(IssueCode::NotAbsolute)),
            ("./models".into(), Err(IssueCode::NotAbsolute)),
            ("~/models".into(), Err(IssueCode::NotAbsolute)),
            ("C:".into(), Err(IssueCode::NotAbsolute)),
            ("C:models".into(), Err(IssueCode::NotAbsolute)),
            ("c:..\\models".into(), Err(IssueCode::NotAbsolute)),
            // UNC and device namespaces
            ("\\\\host\\share".into(), Err(IssueCode::NetworkPath)),
            (
                "\\\\host\\share\\models".into(),
                Err(IssueCode::NetworkPath),
            ),
            ("//host/share".into(), Err(IssueCode::NetworkPath)),
            ("\\\\?\\C:\\data".into(), Err(IssueCode::NetworkPath)),
            ("\\\\.\\COM1".into(), Err(IssueCode::NetworkPath)),
            // device names
            ("C:\\CON".into(), Err(IssueCode::DeviceName)),
            ("C:\\data\\nul.txt".into(), Err(IssueCode::DeviceName)),
            ("/tmp/Aux".into(), Err(IssueCode::DeviceName)),
            ("/tmp/com1".into(), Err(IssueCode::DeviceName)),
            ("/tmp/LPT9.log".into(), Err(IssueCode::DeviceName)),
            ("/tmp/com10".into(), Ok(())),
            ("/tmp/console".into(), Ok(())),
            // roots and system directories
            ("/".into(), Err(IssueCode::SystemDir)),
            ("///".into(), Err(IssueCode::NetworkPath)),
            ("C:\\".into(), Err(IssueCode::SystemDir)),
            ("c:/".into(), Err(IssueCode::SystemDir)),
            ("/etc".into(), Err(IssueCode::SystemDir)),
            ("/etc/hey".into(), Err(IssueCode::SystemDir)),
            ("/usr/local/models".into(), Err(IssueCode::SystemDir)),
            ("/Library/Models".into(), Err(IssueCode::SystemDir)),
            ("/home".into(), Err(IssueCode::SystemDir)),
            ("/home/me".into(), Err(IssueCode::SystemDir)),
            ("/home/me/models".into(), Ok(())),
            ("/Users/me".into(), Err(IssueCode::SystemDir)),
            ("/tmp".into(), Err(IssueCode::SystemDir)),
            ("C:\\Windows".into(), Err(IssueCode::SystemDir)),
            ("c:\\windows\\system32\\x".into(), Err(IssueCode::SystemDir)),
            ("C:\\Program Files\\hey".into(), Err(IssueCode::SystemDir)),
            ("D:\\Program Files (x86)".into(), Err(IssueCode::SystemDir)),
            ("C:\\Users".into(), Err(IssueCode::SystemDir)),
            ("C:\\Users\\me".into(), Err(IssueCode::SystemDir)),
            ("C:\\Users\\me\\models".into(), Ok(())),
            // hostile characters
            ("/tmp/a\u{0}b".into(), Err(IssueCode::ControlChars)),
            ("/tmp/a\nb".into(), Err(IssueCode::ControlChars)),
            ("/tmp/a\u{7}".into(), Err(IssueCode::ControlChars)),
            ("/tmp/a\u{202E}gnp.exe".into(), Err(IssueCode::ControlChars)),
            ("/tmp/\u{2066}x".into(), Err(IssueCode::ControlChars)),
            ("/tmp/a\u{200B}b".into(), Err(IssueCode::ControlChars)),
            ("/tmp/a|b".into(), Err(IssueCode::InvalidFormat)),
            ("/tmp/a*b".into(), Err(IssueCode::InvalidFormat)),
            ("/tmp/a?b".into(), Err(IssueCode::InvalidFormat)),
            (
                "C:\\data\\file:stream".into(),
                Err(IssueCode::InvalidFormat),
            ),
            ("/tmp/trailing.".into(), Err(IssueCode::InvalidFormat)),
            ("/tmp/trailing /x".into(), Err(IssueCode::InvalidFormat)),
        ];
        for (input, want) in cases {
            let got = check_lexical(&input).map(|_| ());
            let shown: String = input.chars().take(60).collect();
            assert_eq!(got, want, "{shown:?}");
        }
    }

    #[test]
    fn cleaned_path_is_trimmed_without_trailing_separator() {
        assert_eq!(check_lexical("  /data/m/ ").unwrap(), "/data/m");
        assert_eq!(check_lexical("C:\\data\\m\\").unwrap(), "C:\\data\\m");
    }

    #[test]
    fn issue_codes_never_carry_the_input() {
        // IssueCode is a fieldless enum: nothing to leak. Guard against a future payload.
        assert_eq!(std::mem::size_of::<IssueCode>(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_rules() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap().to_string();
        // existing directory, and a creatable child (nothing is created)
        assert_eq!(validate_models_dir(&root), Ok(root.clone()));
        let child = format!("{root}/new/deeper");
        assert_eq!(validate_models_dir(&child), Ok(child.clone()));
        assert!(!d.path().join("new").exists());
        // a file in the way, below a file, and a symlink
        let file = d.path().join("f");
        fs::write(&file, b"x").unwrap();
        assert_eq!(
            validate_models_dir(file.to_str().unwrap()),
            Err(IssueCode::NotADirectory)
        );
        assert_eq!(
            validate_models_dir(&format!("{}/sub", file.to_str().unwrap())),
            Err(IssueCode::NotCreatable)
        );
        let link = d.path().join("link");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        assert_eq!(
            validate_models_dir(link.to_str().unwrap()),
            Err(IssueCode::Symlink)
        );
        // a path below a symlinked parent that points into /etc
        assert_eq!(
            validate_models_dir(&format!("{}/hey", link.to_str().unwrap())),
            Err(IssueCode::SystemDir)
        );
        assert_eq!(validate_models_dir(""), Ok(String::new()));
    }
}
