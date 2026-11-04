//! Platform actuators. Windows only (D-005); everything else uses `UnsupportedActuator`.

#[cfg(target_os = "windows")]
pub mod windows;
