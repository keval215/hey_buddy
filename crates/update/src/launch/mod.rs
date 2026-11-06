//! Installer launch (ADR 0005 section 7). The decisions that can be tested anywhere are plain
//! functions here (file name, directory name, command line, ACL string); the Windows-only part
//! that touches the OS is in `windows.rs` (`stage` + `StagedInstaller::launch`) and is UNTESTED
//! on non-Windows machines (it is compile-checked for `x86_64-pc-windows-gnu`).
//!
//! What the launcher never does: take a file name, argument, URL or command from the manifest or
//! the UI; go through the shell, an elevation verb or a command interpreter; or pass any argument
//! at all. The only process API is the one `CreateProcessW` call in `windows.rs`.

use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{stage, StagedInstaller};

/// The only name the installer copy ever has.
pub const SETUP_FILE_NAME: &str = "setup.exe";
/// Directory (below `updates/`) holding the per-launch private directories.
pub const RUN_DIR: &str = "run";
/// Protected DACL, no inheritance from the parent: SYSTEM and the owner (the current user, who
/// creates the directory) get full control, nobody else gets anything. `OICI` lets the file we
/// create inside inherit exactly this.
pub const PRIVATE_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;OW)";

/// Why a stage or launch failed. `code()` is the fixed token shown to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("io_error")]
    Io,
    /// The staged copy does not match the verified size / sha256.
    #[error("hash_mismatch")]
    HashMismatch,
    /// A reparse point / symlink on the way, or a path that cannot be quoted safely.
    #[error("unsafe_path")]
    UnsafePath,
    /// `ERROR_ELEVATION_REQUIRED`: our installer is per-user and never asks for admin.
    #[error("installer_needs_admin")]
    NeedsAdmin,
    /// The OS refused to start it (Smart App Control, antivirus, ...).
    #[error("launch_blocked")]
    Blocked,
    /// This platform has no installer launch.
    #[error("unsupported_platform")]
    Unsupported,
}

impl LaunchError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Io => "io_error",
            Self::HashMismatch => "hash_mismatch",
            Self::UnsafePath => "unsafe_path",
            Self::NeedsAdmin => "installer_needs_admin",
            Self::Blocked => "launch_blocked",
            Self::Unsupported => "unsupported_platform",
        }
    }
}

/// `<16 random bytes>` as 32 lowercase hex digits: the name of one private run directory.
pub fn run_dir_name(random: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    random.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Is `name` exactly a name [`run_dir_name`] makes? Only such directories are ever cleaned up.
pub fn is_run_dir_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<dir>\setup.exe`.
pub fn setup_path(dir: &Path) -> PathBuf {
    dir.join(SETUP_FILE_NAME)
}

/// The whole command line: the quoted executable path and nothing else.
pub fn command_line(exe: &str) -> Result<String, LaunchError> {
    if exe.is_empty() || exe.contains(['"', '\0']) {
        return Err(LaunchError::UnsafePath);
    }
    Ok(format!("\"{exe}\""))
}

/// Is `path` or ANY of its ancestors up to the root a link (symlink / reparse point)? `is_link`
/// answers for one path (a missing path is not a link). Errors from `is_link` propagate, so the
/// caller refuses on anything it cannot read. Pure walk; the Windows stager supplies the probe.
pub fn any_link_in_chain<F>(path: &Path, mut is_link: F) -> Result<bool, LaunchError>
where
    F: FnMut(&Path) -> Result<bool, LaunchError>,
{
    for p in path.ancestors() {
        // The empty path (end of a relative chain) names nothing to probe.
        if p.as_os_str().is_empty() {
            continue;
        }
        if is_link(p)? {
            return Ok(true);
        }
    }
    Ok(false)
}
