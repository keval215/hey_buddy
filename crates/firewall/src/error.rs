//! Error and state types. All messages are fixed strings: never window titles, field values,
//! pixel data or OS error text (L-010).

/// Failure inside a detector or session probe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirewallError {
    #[error("secure-field detection is not supported on this platform")]
    Unsupported,
    #[error("accessibility permission denied")]
    PermissionDenied,
    /// `app` is an app identifier (bundle id / exe name), never a window title.
    #[error("accessibility tree of a visible app could not be read")]
    IncompleteTree { app: String },
    #[error("secure-field detection failed")]
    Detection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    LockScreen,
    AuthPrompt,
    SecureInputActive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    Unsupported,
    PermissionDenied,
    /// Fixed explanatory text only.
    DetectionError(String),
    DetectionTimeout,
    UnstableLayout,
    IncompleteTree {
        app: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Paused(PauseReason),
    Blocked(BlockReason),
}

/// What the session probe sees right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionProbeState {
    Normal,
    LockScreen,
    AuthPrompt,
    SecureInputActive,
}

impl BlockReason {
    pub(crate) fn detection(text: &str) -> Self {
        BlockReason::DetectionError(text.to_string())
    }
}

impl From<FirewallError> for BlockReason {
    fn from(e: FirewallError) -> Self {
        match e {
            FirewallError::Unsupported => BlockReason::Unsupported,
            FirewallError::PermissionDenied => BlockReason::PermissionDenied,
            FirewallError::IncompleteTree { app } => BlockReason::IncompleteTree { app },
            FirewallError::Detection => BlockReason::detection("secure-field detection failed"),
        }
    }
}
