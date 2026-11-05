//! Fixed-token errors. `Display` is the token; no path, label or OS text ever appears.

/// Why an audit operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogError {
    /// The log is not set up (no data directory, or setup failed). Fail closed.
    #[error("unavailable")]
    Unavailable,
    /// The audit directory or a segment is a symlink / reparse point, or not a directory/file.
    #[error("symlink")]
    Symlink,
    /// A destructive command was called without `confirm: true`.
    #[error("needs_confirm")]
    NeedsConfirm,
    /// The export destination already exists.
    #[error("exists")]
    Exists,
    /// The export destination is not acceptable (inside the audit directory, no parent ...).
    #[error("invalid_path")]
    InvalidPath,
    #[error("io_error")]
    Io,
}

impl LogError {
    /// The fixed token (same as `Display`).
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Symlink => "symlink",
            Self::NeedsConfirm => "needs_confirm",
            Self::Exists => "exists",
            Self::InvalidPath => "invalid_path",
            Self::Io => "io_error",
        }
    }
}
