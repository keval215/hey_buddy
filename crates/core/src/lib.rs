//! hey_buddy `core` crate (package `buddy-core`): wiring and app-level logic.

pub mod api;
pub mod buddy;
pub mod consent;
pub mod error;
pub mod point;
pub mod route;
pub mod settings;

pub use buddy::Buddy;

/// Hide one of our own windows (overlay, settings, panel) from all screen capture.
/// Windows only; macOS excludes our whole process by pid inside the capture crate.
#[cfg(target_os = "windows")]
pub use capture::exclude_window_from_capture;

/// The capture source for this OS (the app has no direct dependency on `capture`).
pub fn platform_capture() -> Box<dyn capture::CaptureSource> {
    capture::platform_capture()
}
