//! Windows: UI Automation `IsPassword` detection and lock/secure-desktop probe.
//! UNTESTED on this machine (compile-checked only, `x86_64-pc-windows-gnu`).
//! See [`WinDetector`] for coordinate conventions, DPI requirement and known limitations.

use std::time::Duration;

use frame::Rect;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_IsPasswordPropertyId,
};
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
    DPI_AWARENESS_PER_MONITOR_AWARE,
};

use crate::{FirewallError, SecureFieldDetector, SessionProbe, SessionProbeState};

/// Deadline for the UIA search (`FindAll` over the desktop root). Longer than the 200 ms
/// default because a descendant search over every window is slow. Real-world latency is
/// UNVERIFIED; tune on real machines. A scoped search (per visible top-level window) would be
/// cheaper but was not implemented.
pub const DETECTOR_TIMEOUT: Duration = Duration::from_millis(1500);

/// Coordinate space: UIA reports bounding rectangles in the virtual-desktop coordinates of the
/// calling process. For a per-monitor-DPI-aware process those are PHYSICAL pixels, which is
/// exactly what `capture::windows_wgc` uses for `DisplayInfo.origin` (scale `1.0`). So the rects
/// are returned unchanged: no per-display or per-monitor scaling is applied, which keeps them
/// independent of any display and identical between preflight and check on HiDPI and
/// multi-monitor setups.
///
/// REQUIREMENT: the desktop app MUST declare per-monitor DPI awareness (PerMonitorV2) in its
/// application manifest (`<dpiAwareness>PerMonitorV2</dpiAwareness>`). If the calling thread is
/// not per-monitor aware the rects would be virtualised (wrong), so the detector fails closed
/// with `Detection`.
///
/// # Known limitations
/// - No incomplete-tree detection: Chromium/Electron build their UIA tree lazily, so a UIA
///   client can silently get NO password fields from such an app. The Windows detector cannot
///   tell this from "no password fields" and does not report `IncompleteTree`.
/// - The session probe never reports `AuthPrompt`; UAC/secure desktop shows up as `LockScreen`.
/// - Only elements with `IsPassword == true` are found; hidden/offscreen password elements
///   with a zero rect make the firewall block (fail closed).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinDetector;

fn to_u32(v: i64) -> Result<u32, FirewallError> {
    u32::try_from(v).map_err(|_| FirewallError::Detection)
}

impl SecureFieldDetector for WinDetector {
    /// Returns bounding rects of all password elements under the desktop root (see the type
    /// docs for the coordinate space). Any failure to enumerate or read an element => `Detection`
    /// (blocks); a zero/negative-size rect is returned as zero-area, which the firewall blocks.
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError> {
        // SAFETY: plain COM/Win32 calls on a thread-local apartment; every HRESULT is checked
        // and mapped to a fixed-text error (no OS text is forwarded).
        unsafe {
            let ctx = GetThreadDpiAwarenessContext();
            if GetAwarenessFromDpiAwarenessContext(ctx) != DPI_AWARENESS_PER_MONITOR_AWARE {
                return Err(FirewallError::Detection);
            }
            // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is already usable on this thread.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|_| FirewallError::Detection)?;
            let root = auto
                .GetRootElement()
                .map_err(|_| FirewallError::Detection)?;
            let cond = auto
                .CreatePropertyCondition(UIA_IsPasswordPropertyId, &VARIANT::from(true))
                .map_err(|_| FirewallError::Detection)?;
            let found = root
                .FindAll(TreeScope_Descendants, &cond)
                .map_err(|_| FirewallError::Detection)?;
            let n = found.Length().map_err(|_| FirewallError::Detection)?;
            let mut out = Vec::with_capacity(n.max(0) as usize);
            for i in 0..n {
                let el = found.GetElement(i).map_err(|_| FirewallError::Detection)?;
                let r = el
                    .CurrentBoundingRectangle()
                    .map_err(|_| FirewallError::Detection)?;
                let w = to_u32(r.right as i64 - r.left as i64)?;
                let h = to_u32(r.bottom as i64 - r.top as i64)?;
                out.push(Rect::new(r.left, r.top, w, h));
            }
            Ok(out)
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WinSessionProbe;

impl SessionProbe for WinSessionProbe {
    /// `OpenInputDesktop` fails when the input desktop is not ours: the lock screen, UAC consent
    /// or Ctrl+Alt+Del screen (Winlogon secure desktop). Any such failure => `LockScreen` pause.
    fn state(&self) -> Result<SessionProbeState, FirewallError> {
        // SAFETY: handle is closed immediately; no pointers escape.
        unsafe {
            match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) {
                Ok(h) => {
                    let _ = CloseDesktop(h);
                    Ok(SessionProbeState::Normal)
                }
                Err(_) => Ok(SessionProbeState::LockScreen),
            }
        }
    }
}
