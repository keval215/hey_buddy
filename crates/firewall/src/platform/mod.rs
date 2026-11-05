//! Platform detectors and probes. Each OS has one module; every other OS gets `Unsupported*`,
//! which fails closed.

use frame::Rect;

use crate::{FirewallError, SecureFieldDetector, SessionProbe, SessionProbeState};

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
mod macos_text;
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
mod role_map;
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
mod walk;
#[cfg(any(test, target_os = "windows"))]
mod win_pure;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
mod windows_agent;
#[cfg(target_os = "windows")]
mod windows_text;

/// Detector for platforms without an implementation: always errors (=> `Blocked(Unsupported)`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedDetector;

impl SecureFieldDetector for UnsupportedDetector {
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSessionProbe;

impl SessionProbe for UnsupportedSessionProbe {
    fn state(&self) -> Result<SessionProbeState, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

/// The firewall for this OS. On unsupported OSes every call is `Blocked(Unsupported)`.
pub fn platform_firewall() -> Box<dyn crate::Firewall> {
    #[cfg(target_os = "macos")]
    {
        Box::new(
            crate::PrivacyFirewall::new(macos::MacDetector, macos::MacSessionProbe)
                .with_detector_timeout(macos::DETECTOR_TIMEOUT)
                .with_text_source(macos_text::MacTextSource),
        )
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(
            crate::PrivacyFirewall::new(windows::WinDetector, windows::WinSessionProbe)
                .with_detector_timeout(windows::DETECTOR_TIMEOUT)
                .with_text_source(windows_text::WinTextSource)
                .with_observation(
                    windows_agent::WinObservationSource,
                    windows_agent::WinTargetResolver,
                ),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(crate::PrivacyFirewall::new(
            UnsupportedDetector,
            UnsupportedSessionProbe,
        ))
    }
}
