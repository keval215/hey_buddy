//! macOS text source (T13): reads the AX tree of the FOCUSED window of the FRONTMOST app only.
//! UNTESTED on this machine (compile-checked only, `aarch64-apple-darwin`); the pure walker in
//! `walk.rs` and the role tables in `role_map.rs` are unit-tested on Linux.
//!
//! What is read, per element, in this order:
//! 1. `AXRole` / `AXSubrole` (via the SAME `macos::is_secure` as the T8 detector). Secure
//!    elements: `AXPosition` + `AXSize` (same `macos::frame_of`) ONLY; no other attribute, no
//!    children.
//! 2. `AXPosition`/`AXSize`, `AXRole` (mapping), and for non-secure elements `AXChildren`.
//! 3. Labels (not for the window or `AXWebArea`): first non-empty of `AXTitle`,
//!    `AXDescription`, `AXPlaceholderValue`. The window's own `AXTitle` is never read.
//! 4. `AXValue`: for editable text elements only its emptiness (CFString length; the object is
//!    dropped at once, the text is never copied); for `AXStaticText`/`AXHeading` the string.
//!
//! The frontmost app is `AXFocusedApplication` of the system-wide element; its window is
//! `AXFocusedWindow`, falling back to `AXMainWindow`; none => `Err`. The app id is the bundle
//! identifier resolved from the executable path of the pid (outermost `.app`), which is the id
//! the capture deny-list matches. Unresolvable => empty string (the firewall withholds).
//!
//! Chromium/Electron: if the tree is unreadable or has only the root, `AXManualAccessibility`
//! and `AXEnhancedUserInterface` are set (same helper as T8) and the read is retried every
//! 50 ms (6 times, inside the overall 1200 ms budget); still root-only => returned as is (the
//! firewall treats it as `IncompleteTree`, voice-only).

use std::ffi::c_void;
use std::os::raw::c_int;
use std::time::Instant;

use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFGetTypeID, CFRetain};
use core_foundation_sys::bundle::{CFBundleCreate, CFBundleGetIdentifier};
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringGetCString, CFStringGetLength, CFStringGetTypeID, CFStringRef,
};
use core_foundation_sys::url::CFURLCreateFromFileSystemRepresentation;
use frame::Rect;

use super::macos::{
    attr, enable_accessibility, frame_of, is_secure, proc_pidpath, AXError, AXUIElementRef, Cf,
    AX_API_DISABLED, AX_WAIT_STEP, AX_WAIT_STEPS,
};
use super::role_map::{ax_role, ax_role_is_editable};
use super::walk::{walk_tree, Limits, TreeCursor};
use crate::{FirewallError, NodeRole, RawWindowTree, ScreenTextSource};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementGetPid(el: AXUIElementRef, pid: *mut c_int) -> AXError;
}
extern "C" {
    fn getpid() -> c_int;
}

fn map_err(e: AXError) -> FirewallError {
    if e == AX_API_DISABLED {
        FirewallError::PermissionDenied
    } else {
        FirewallError::Detection
    }
}

/// Larger string reader than `macos::string_of` (labels/static text up to ~500 chars). A string
/// that does not fit is dropped (`None`), never half-read.
fn long_string(v: &Cf) -> Option<String> {
    // SAFETY: type checked before the cast; buffer sized and NUL-terminated by CF.
    unsafe {
        if CFGetTypeID(v.0) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = vec![0 as std::os::raw::c_char; 4096];
        if CFStringGetCString(
            v.0 as CFStringRef,
            buf.as_mut_ptr(),
            buf.len() as _,
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

fn str_attr(el: AXUIElementRef, name: &str) -> Result<Option<String>, FirewallError> {
    Ok(attr(el, name)
        .map_err(map_err)?
        .and_then(|v| long_string(&v)))
}

/// Cursor over AX elements. Nodes are owned (retained) element refs.
struct AxCursor;

impl TreeCursor for AxCursor {
    type Node = Cf;

    fn is_secure(&mut self, n: &Cf) -> Result<bool, FirewallError> {
        is_secure(n.0).map_err(map_err)
    }
    fn rect(&mut self, n: &Cf) -> Result<Option<Rect>, FirewallError> {
        frame_of(n.0).map_err(map_err)
    }
    fn role(&mut self, n: &Cf) -> Result<NodeRole, FirewallError> {
        let role = str_attr(n.0, "AXRole")?.unwrap_or_default();
        let sub = str_attr(n.0, "AXSubrole")?;
        Ok(ax_role(&role, sub.as_deref()))
    }
    fn is_editable(&mut self, n: &Cf) -> Result<bool, FirewallError> {
        let role = str_attr(n.0, "AXRole")?.unwrap_or_default();
        Ok(ax_role_is_editable(&role))
    }
    fn has_value(&mut self, n: &Cf) -> Result<bool, FirewallError> {
        // Emptiness only: the value object is dropped at the end of this function and its text
        // is never copied.
        let Some(v) = attr(n.0, "AXValue").map_err(map_err)? else {
            return Ok(false);
        };
        // SAFETY: type checked before the CFString call.
        unsafe {
            if CFGetTypeID(v.0) == CFStringGetTypeID() {
                Ok(CFStringGetLength(v.0 as CFStringRef) > 0)
            } else {
                Ok(true)
            }
        }
    }
    fn label(&mut self, n: &Cf) -> Result<Option<String>, FirewallError> {
        for name in ["AXTitle", "AXDescription", "AXPlaceholderValue"] {
            if let Some(s) = str_attr(n.0, name)? {
                if !s.trim().is_empty() {
                    return Ok(Some(s));
                }
            }
        }
        Ok(None)
    }
    fn text_value(&mut self, n: &Cf) -> Result<Option<String>, FirewallError> {
        str_attr(n.0, "AXValue")
    }
    fn children(&mut self, n: &Cf) -> Result<Vec<Cf>, FirewallError> {
        let Some(kids) = attr(n.0, "AXChildren").map_err(map_err)? else {
            return Ok(Vec::new());
        };
        let arr = kids.0 as CFArrayRef;
        let mut out = Vec::new();
        // SAFETY: AXChildren is a CFArray of AXUIElementRefs kept alive by `kids`; each child is
        // retained so the returned `Cf` owns it.
        unsafe {
            for i in 0..CFArrayGetCount(arr) {
                let c = CFArrayGetValueAtIndex(arr, i);
                if c.is_null() {
                    return Err(FirewallError::Detection);
                }
                out.push(Cf(CFRetain(c)));
            }
        }
        Ok(out)
    }
}

/// (focused window, pid) of the frontmost app. Only this window is ever walked.
fn focused_window() -> Result<(Cf, i32), FirewallError> {
    // SAFETY: returns an owned element (or null).
    let sys = unsafe { AXUIElementCreateSystemWide() };
    if sys.is_null() {
        return Err(FirewallError::Detection);
    }
    let sys = Cf(sys);
    let app = attr(sys.0, "AXFocusedApplication")
        .map_err(map_err)?
        .ok_or(FirewallError::Detection)?;
    let mut pid: c_int = 0;
    // SAFETY: valid element, valid out pointer.
    if unsafe { AXUIElementGetPid(app.0, &mut pid) } != 0 || pid <= 0 {
        return Err(FirewallError::Detection);
    }
    let win = match attr(app.0, "AXFocusedWindow").map_err(map_err)? {
        Some(w) => w,
        None => attr(app.0, "AXMainWindow")
            .map_err(map_err)?
            .ok_or(FirewallError::Detection)?,
    };
    Ok((win, pid))
}

/// Bundle identifier of the app owning `pid`, from its executable path (outermost `.app`).
fn bundle_id(pid: i32) -> Option<String> {
    let mut buf = [0u8; 4096];
    // SAFETY: buffer pointer and length match.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = &buf[..n as usize];
    let end = path.windows(5).position(|w| w == b".app/")? + 4;
    let bundle_path = &path[..end];
    // SAFETY: valid byte buffer; created objects are owned by `Cf`; the identifier is borrowed
    // from the bundle, which outlives the copy below.
    unsafe {
        let url = Cf(CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bundle_path.as_ptr(),
            bundle_path.len() as _,
            1,
        ) as _);
        if url.0.is_null() {
            return None;
        }
        let bundle = Cf(CFBundleCreate(std::ptr::null(), url.0 as _) as _);
        if bundle.0.is_null() {
            return None;
        }
        let id = CFBundleGetIdentifier(bundle.0 as _);
        if id.is_null() {
            return None;
        }
        let id = Cf(CFRetain(id as _));
        long_string(&id).filter(|s| !s.is_empty())
    }
}

fn read_window(pid: i32, app_id: &str, limits: &Limits) -> Result<RawWindowTree, FirewallError> {
    let (win, wpid) = focused_window()?;
    if wpid != pid {
        return Err(FirewallError::Detection); // focus moved: never mix two apps
    }
    walk_tree(&mut AxCursor, win, app_id.to_string(), limits)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacTextSource;

impl ScreenTextSource for MacTextSource {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError> {
        // SAFETY: no arguments.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(FirewallError::PermissionDenied);
        }
        let limits = Limits::standard(Instant::now());
        let (_, pid) = focused_window()?;
        // SAFETY: no arguments.
        if pid == unsafe { getpid() } {
            // Our own overlay/panel: voice-only.
            return Err(FirewallError::IncompleteTree { app: String::new() });
        }
        let app_id = bundle_id(pid).unwrap_or_default();

        let usable =
            |r: &Result<RawWindowTree, FirewallError>| matches!(r, Ok(t) if t.nodes.len() > 1);
        let mut result = read_window(pid, &app_id, &limits);
        if !usable(&result) && !matches!(result, Err(FirewallError::PermissionDenied)) {
            enable_accessibility(pid);
            for _ in 0..AX_WAIT_STEPS {
                if Instant::now() >= limits.deadline {
                    break;
                }
                std::thread::sleep(AX_WAIT_STEP);
                result = read_window(pid, &app_id, &limits);
                if usable(&result) || matches!(result, Err(FirewallError::PermissionDenied)) {
                    break;
                }
            }
        }
        let tree = result?;
        // Focus must not have moved to another app while we read.
        match focused_window() {
            Ok((_, p)) if p == pid => Ok(tree),
            _ => Err(FirewallError::Detection),
        }
    }
}
