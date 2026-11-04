//! hey_buddy `core` crate (package `buddy-core`): wiring and app-level logic.

pub mod activity;
pub mod agent;
pub mod api;
pub mod approval;
pub mod audit_log;
pub mod buddy;
pub mod consent;
pub mod deny_edit;
pub mod download;
pub mod engine_slot;
pub mod error;
pub mod events;
pub mod gateway;
pub mod kill;
pub mod kill_hotkey;
pub mod leakguard;
pub mod models_path;
pub mod point;
pub mod prompt;
pub mod providers;
#[cfg(feature = "real-voice")]
pub mod real_voice;
pub mod route;
pub mod settings;
pub mod setup;
pub mod spoken;
pub mod turn;
pub mod update;

pub use buddy::Buddy;
pub use buddy::SwapError;
/// Re-exported so the app shell can name them without depending on the leaf crates.
pub use capture::DenyList;
pub use engine_slot::{EngineLoader, EngineSlot, SwapFailure};
pub use events::{EventSink, NullSink};
pub use setup::{load_deny_list, platform_firewall, DenyListSetup};
pub use turn::{EngineDeps, ProviderSource, VadFactory};
pub use voice::VoiceError;

/// Hide one of our own windows (overlay, settings, panel) from all screen capture.
/// Windows only; macOS excludes our whole process by pid inside the capture crate.
#[cfg(target_os = "windows")]
pub use capture::exclude_window_from_capture;

/// The capture source for this OS (the app has no direct dependency on `capture`).
pub fn platform_capture() -> Box<dyn capture::CaptureSource> {
    capture::platform_capture()
}
