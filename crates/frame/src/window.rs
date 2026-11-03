use crate::Rect;

/// A visible top-level window. The title is intentionally NOT stored: titles can contain
/// private content and must never reach logs, errors or models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// App identifier: bundle id on macOS, exe file name on Windows.
    pub app_id: String,
    /// Process id.
    pub pid: u32,
    /// Window bounds in global points.
    pub rect: Rect,
}
