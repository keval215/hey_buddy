//! Screen capture behind a small trait, with an app deny-list applied AT CAPTURE TIME.
//!
//! Deny-listed windows are excluded from the capture itself (never blurred afterwards).
//! Errors never contain window titles or screen content. A failed capture returns an error,
//! never a partial or unfiltered frame.

use async_trait::async_trait;
use frame::{DisplayId, DisplayInfo, Frame};

pub mod deny;
pub mod policy;

#[cfg(any(test, feature = "fakes"))]
pub mod fake;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows_wgc;

pub use deny::{DenyList, DenyListError, LoadedDenyList};
pub use policy::{filter_windows, windows_policy, Decision};
#[cfg(target_os = "windows")]
pub use windows_wgc::exclude_window_from_capture;

/// What part of the display to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureScope {
    /// Whole display.
    Display,
    /// Only the foreground window (Windows fallback when a deny-listed app is visible).
    ForegroundWindow,
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub display: DisplayId,
    pub deny_list: DenyList,
    pub scope: CaptureScope,
}

/// Fixed messages only: no window titles, no screen content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("screen capture is not supported on this platform")]
    Unsupported,
    #[error("screen capture permission denied")]
    PermissionDenied,
    /// `app` is an application identifier (bundle id / exe name), never a window title.
    #[error("a deny-listed app is visible: {app}")]
    DeniedAppVisible { app: String },
    #[error("platform capture error: {0}")]
    Platform(String),
    #[error("screen capture timed out")]
    Timeout,
}

#[async_trait]
pub trait CaptureSource: Send + Sync {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError>;
    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError>;
}

/// Capture for OSes without an implementation: always `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedCapture;

#[async_trait]
impl CaptureSource for UnsupportedCapture {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        Err(CaptureError::Unsupported)
    }
    async fn capture(&self, _req: &CaptureRequest) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unsupported)
    }
}

/// The capture implementation for this OS.
pub fn platform_capture() -> Box<dyn CaptureSource> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacCapture::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows_wgc::WindowsCapture::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(UnsupportedCapture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_returns_unsupported() {
        let c = UnsupportedCapture;
        assert_eq!(c.displays().unwrap_err(), CaptureError::Unsupported);
        let req = CaptureRequest {
            display: DisplayId(1),
            deny_list: DenyList::defaults(),
            scope: CaptureScope::Display,
        };
        assert_eq!(
            futures::executor::block_on(c.capture(&req)).unwrap_err(),
            CaptureError::Unsupported
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn platform_capture_is_unsupported_on_linux() {
        assert_eq!(
            platform_capture().displays().unwrap_err(),
            CaptureError::Unsupported
        );
    }

    #[test]
    fn error_strings_are_fixed() {
        for e in [
            CaptureError::Unsupported,
            CaptureError::PermissionDenied,
            CaptureError::Timeout,
        ] {
            assert!(!e.to_string().is_empty());
        }
        let s = CaptureError::DeniedAppVisible {
            app: "com.x.y".into(),
        }
        .to_string();
        assert!(s.contains("com.x.y") && !s.to_lowercase().contains("title"));
    }
}
