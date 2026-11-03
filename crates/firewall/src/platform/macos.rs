//! macOS: Accessibility (AX) secure-field detection and session probe.
//! UNTESTED on this machine: not compiled on Linux. Uses raw FFI to ApplicationServices /
//! Carbon plus `core-foundation-sys` (MIT/Apache-2.0).
//!
//! Detection walks the AX tree of every on-screen app and collects elements whose role OR
//! subrole is `AXSecureTextField` (subrole is checked on every element, any role), with frames
//! from `AXPosition` + `AXSize` (global points, the same space as capture's `DisplayInfo`). The
//! detector is display-independent. A secure field whose size/position is NaN, infinite, zero
//! or negative is an error (fail closed), never an empty rect.
//!
//! If an app's tree cannot be read, or it has an on-screen window but yields zero AX nodes, the
//! detector sets `AXManualAccessibility` (Chromium/Electron) and `AXEnhancedUserInterface` =
//! true on that app, then polls (50 ms steps, at most ~300 ms in total) for a readable tree;
//! still unreadable => `IncompleteTree { app }`.
//!
//! # Known limitations (all UNVERIFIED on real Macs)
//! - `AuthPrompt` is never reported by the session probe (only lock screen / login window and
//!   secure event input).
//! - Chromium/Electron build their AX tree asynchronously; the bounded wait after enabling
//!   accessibility may be too short, which then yields `IncompleteTree` (blocks), or, worse for
//!   completeness, a partially built tree.
//! - Apps owned by a small allow-list of SIP-protected system processes (see `NO_SECURE_OWNERS`)
//!   are not scanned; this list has not been verified on real systems.

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef};
use core_foundation_sys::number::kCFBooleanTrue;
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringCreateWithCString, CFStringGetCString, CFStringGetTypeID,
    CFStringRef,
};
use frame::Rect;

use crate::{FirewallError, SecureFieldDetector, SessionProbe, SessionProbeState};

type AXUIElementRef = *const c_void;
type AXError = i32;
type AXValueRef = *const c_void;
const AX_OK: AXError = 0;
const AX_API_DISABLED: AXError = -25211;
const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 20_000;
/// Detector deadline (bounded accessibility wait of ~300 ms per unreadable app plus the walk).
pub const DETECTOR_TIMEOUT: Duration = Duration::from_millis(2500);
const AX_WAIT_STEP: Duration = Duration::from_millis(50);
const AX_WAIT_STEPS: u32 = 6; // 6 * 50 ms = 300 ms

/// System owners (by `kCGWindowOwnerName`) that cannot host secure text fields and are not
/// scanned. A name alone is spoofable, so the owner's executable must also live under `/System/`
/// (SIP-protected). `loginwindow` is deliberately NOT listed: the session probe handles it.
/// UNVERIFIED on real Macs.
const NO_SECURE_OWNERS: &[&str] = &[
    "Window Server",
    "Dock",
    "Control Center",
    "SystemUIServer",
    "Notification Center",
];

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: c_int) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        el: AXUIElementRef,
        attr: CFStringRef,
        out: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(el: AXUIElementRef, attr: CFStringRef, v: CFTypeRef)
        -> AXError;
    fn AXValueGetValue(v: AXValueRef, ty: u32, out: *mut c_void) -> bool;
}
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
}
extern "C" {
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
}

/// Owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own one retain count on a valid CF object.
            unsafe { CFRelease(self.0) }
        }
    }
}

fn cfstr(s: &str) -> Option<Cf> {
    let c = std::ffi::CString::new(s).ok()?;
    // SAFETY: valid NUL-terminated string; result owned by us.
    let r =
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) };
    (!r.is_null()).then(|| Cf(r as CFTypeRef))
}

fn attr(el: AXUIElementRef, name: &str) -> Result<Option<Cf>, AXError> {
    let key = cfstr(name).ok_or(-1)?;
    let mut out: CFTypeRef = std::ptr::null();
    // SAFETY: valid element and key; `out` receives an owned object on success.
    let e = unsafe { AXUIElementCopyAttributeValue(el, key.0 as CFStringRef, &mut out) };
    match e {
        AX_OK if !out.is_null() => Ok(Some(Cf(out))),
        AX_OK => Ok(None),
        // kAXErrorNoValue / AttributeUnsupported: the attribute simply does not exist.
        -25212 | -25205 => Ok(None),
        e => Err(e),
    }
}

fn string_of(v: &Cf) -> Option<String> {
    // SAFETY: type checked before the cast; buffer sized and NUL-terminated by CF.
    unsafe {
        if CFGetTypeID(v.0) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = [0 as c_char; 256];
        if CFStringGetCString(
            v.0 as CFStringRef,
            buf.as_mut_ptr(),
            256,
            kCFStringEncodingUTF8,
        ) == 0
        {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn frame_of(el: AXUIElementRef) -> Result<Option<Rect>, AXError> {
    let (Some(p), Some(s)) = (attr(el, "AXPosition")?, attr(el, "AXSize")?) else {
        return Ok(None);
    };
    let (mut pt, mut sz) = (CGPoint::default(), CGSize::default());
    // SAFETY: out pointers match the requested AXValue types.
    let ok = unsafe {
        AXValueGetValue(p.0, AX_VALUE_CGPOINT, &mut pt as *mut _ as *mut c_void)
            && AXValueGetValue(s.0, AX_VALUE_CGSIZE, &mut sz as *mut _ as *mut c_void)
    };
    if !ok
        || !pt.x.is_finite()
        || !pt.y.is_finite()
        || !sz.width.is_finite()
        || !sz.height.is_finite()
        || sz.width <= 0.0
        || sz.height <= 0.0
    {
        return Err(-1);
    }
    // Round outward so the rect always covers the field.
    let x0 = pt.x.floor();
    let y0 = pt.y.floor();
    let w = (pt.x + sz.width).ceil() - x0;
    let h = (pt.y + sz.height).ceil() - y0;
    let lim = i32::MAX as f64;
    if !(x0.abs() < lim && y0.abs() < lim && w >= 1.0 && h >= 1.0 && w < lim && h < lim) {
        return Err(-1);
    }
    Ok(Some(Rect::new(x0 as i32, y0 as i32, w as u32, h as u32)))
}

fn is_secure(el: AXUIElementRef) -> Result<bool, AXError> {
    let role = attr(el, "AXRole")?.and_then(|v| string_of(&v));
    if role.as_deref() == Some("AXSecureTextField") {
        return Ok(true);
    }
    // Subrole is checked on every element, whatever its role.
    let sub = attr(el, "AXSubrole")?.and_then(|v| string_of(&v));
    Ok(sub.as_deref() == Some("AXSecureTextField"))
}

fn walk(
    el: AXUIElementRef,
    depth: usize,
    nodes: &mut usize,
    out: &mut Vec<Rect>,
) -> Result<(), AXError> {
    *nodes += 1;
    if depth > MAX_DEPTH || *nodes > MAX_NODES {
        return Err(-1); // too deep/large to cover fully: treat as incomplete
    }
    if is_secure(el)? {
        // A secure field whose frame is unknown cannot be hidden: error, not skip.
        out.push(frame_of(el)?.ok_or(-1)?);
    }
    if let Some(children) = attr(el, "AXChildren")? {
        let arr = children.0 as CFArrayRef;
        // SAFETY: AXChildren is a CFArray of AXUIElementRefs kept alive by `children`.
        let n = unsafe { CFArrayGetCount(arr) };
        for i in 0..n {
            let child = unsafe { CFArrayGetValueAtIndex(arr, i) } as AXUIElementRef;
            walk(child, depth + 1, nodes, out)?;
        }
    }
    Ok(())
}

/// Returns the secure-field rects and the number of AX nodes visited.
fn scan_app(pid: i32) -> Result<(Vec<Rect>, usize), AXError> {
    // SAFETY: returns an owned element (or null).
    let app = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        return Err(-1);
    }
    let app = Cf(app);
    let mut rects = Vec::new();
    let mut nodes = 0;
    let windows = attr(app.0, "AXWindows")?;
    if let Some(w) = windows {
        let arr = w.0 as CFArrayRef;
        let n = unsafe { CFArrayGetCount(arr) };
        for i in 0..n {
            let win = unsafe { CFArrayGetValueAtIndex(arr, i) } as AXUIElementRef;
            walk(win, 0, &mut nodes, &mut rects)?;
        }
    }
    Ok((rects, nodes))
}

/// Ask a Chromium/Electron app to build its accessibility tree.
fn enable_accessibility(pid: i32) {
    // SAFETY: owned app element; attribute names are valid CF strings; kCFBooleanTrue is static.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }
        let app = Cf(app);
        for name in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
            if let Some(k) = cfstr(name) {
                let _ =
                    AXUIElementSetAttributeValue(app.0, k.0 as CFStringRef, kCFBooleanTrue as _);
            }
        }
    }
}

/// True if the executable of `pid` lives under `/System/` (SIP-protected). Unknown => false.
fn is_system_path(pid: i32) -> bool {
    let mut buf = [0u8; 4096];
    // SAFETY: buffer pointer and length match; the call writes at most `size` bytes.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    n > 0 && buf[..n as usize].starts_with(b"/System/")
}

/// (pid, app name) of every app owning an on-screen window, from the CoreGraphics window list.
/// The name is the owner app name, never a window title.
fn visible_pids() -> Result<Vec<(i32, String)>, FirewallError> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, rel: u32) -> CFTypeRef;
    }
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    // SAFETY: owned CFArray of CFDictionary; keys are CF strings we create.
    unsafe {
        let list = Cf(CGWindowListCopyWindowInfo(
            ON_SCREEN_ONLY | EXCLUDE_DESKTOP,
            0,
        ));
        if list.0.is_null() {
            return Err(FirewallError::Detection);
        }
        let pid_key = cfstr("kCGWindowOwnerPID").ok_or(FirewallError::Detection)?;
        let name_key = cfstr("kCGWindowOwnerName").ok_or(FirewallError::Detection)?;
        let arr = list.0 as CFArrayRef;
        let mut seen = std::collections::BTreeMap::new();
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i) as CFTypeRef;
            let pidv = CFDictionaryGetValue(d, pid_key.0);
            if pidv.is_null() {
                return Err(FirewallError::Detection);
            }
            let mut pid: i32 = 0;
            if !core_foundation_sys::number::CFNumberGetValue(
                pidv as _,
                core_foundation_sys::number::kCFNumberSInt32Type,
                &mut pid as *mut _ as *mut c_void,
            ) {
                return Err(FirewallError::Detection);
            }
            // Owner name is an app name, not a window title.
            let name = CFDictionaryGetValue(d, name_key.0);
            let name = if name.is_null() {
                String::from("unknown")
            } else {
                let tmp = Cf(core_foundation_sys::base::CFRetain(name));
                string_of(&tmp).unwrap_or_else(|| "unknown".into())
            };
            if NO_SECURE_OWNERS.contains(&name.as_str()) && is_system_path(pid) {
                continue;
            }
            seen.insert(pid, name);
        }
        Ok(seen.into_iter().collect())
    }
}

/// Scan one app; if unreadable or empty, enable accessibility and poll for a tree.
fn scan_with_retry(pid: i32) -> Result<Vec<Rect>, FirewallError> {
    match scan_app(pid) {
        Ok((r, n)) if n > 0 => return Ok(r),
        Err(AX_API_DISABLED) => return Err(FirewallError::PermissionDenied),
        // Unreadable, or an app with an on-screen window but zero AX nodes: ask for a tree.
        _ => {}
    }
    enable_accessibility(pid);
    for _ in 0..AX_WAIT_STEPS {
        std::thread::sleep(AX_WAIT_STEP);
        match scan_app(pid) {
            Ok((r, n)) if n > 0 => return Ok(r),
            Err(AX_API_DISABLED) => return Err(FirewallError::PermissionDenied),
            _ => {}
        }
    }
    Err(FirewallError::IncompleteTree { app: String::new() })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacDetector;

impl SecureFieldDetector for MacDetector {
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError> {
        // SAFETY: no arguments.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(FirewallError::PermissionDenied);
        }
        let mut all = Vec::new();
        for (pid, app) in visible_pids()? {
            match scan_with_retry(pid) {
                Ok(r) => all.extend(r),
                Err(FirewallError::IncompleteTree { .. }) => {
                    return Err(FirewallError::IncompleteTree { app })
                }
                Err(e) => return Err(e),
            }
        }
        Ok(all)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacSessionProbe;

impl SessionProbe for MacSessionProbe {
    fn state(&self) -> Result<SessionProbeState, FirewallError> {
        // SAFETY: CGSessionCopyCurrentDictionary returns an owned dictionary or null.
        unsafe {
            let d = Cf(CGSessionCopyCurrentDictionary());
            if d.0.is_null() {
                // No console session dictionary: login window / fast user switch. Fail closed.
                return Ok(SessionProbeState::LockScreen);
            }
            // CGSSessionScreenIsLocked is present (true) only while the screen is locked.
            let k = cfstr("CGSSessionScreenIsLocked").ok_or(FirewallError::Detection)?;
            let v = CFDictionaryGetValue(d.0, k.0);
            if !v.is_null() && v == kCFBooleanTrue as CFTypeRef {
                return Ok(SessionProbeState::LockScreen);
            }
            // Not on console (login window / user switched away): fail closed as lock screen.
            let k = cfstr("kCGSSessionOnConsoleKey").ok_or(FirewallError::Detection)?;
            if CFDictionaryGetValue(d.0, k.0).is_null() {
                return Ok(SessionProbeState::LockScreen);
            }
            if IsSecureEventInputEnabled() {
                return Ok(SessionProbeState::SecureInputActive);
            }
        }
        Ok(SessionProbeState::Normal)
    }
}
