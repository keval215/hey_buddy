//! Windows text source (T13): reads the UIA tree of the FOREGROUND window only.
//! UNTESTED on this machine (compile-checked only, `x86_64-pc-windows-gnu`); the pure walker in
//! `walk.rs` and the role tables in `role_map.rs` are unit-tested on Linux.
//!
//! `GetForegroundWindow` -> `ElementFromHandle` -> walk that element's subtree only (children
//! scope, one level at a time). A foreground window of our own process => `Err` (voice-only).
//! The foreground window is re-checked after the walk; a change => `Err`.
//!
//! What is read, per element, in this order:
//! 1. `IsPassword` (same `windows::is_password` as the T8 detector). Secure elements: bounding
//!    rect ONLY (same `windows::bounding_rect`); no name, no value, no children.
//! 2. Bounding rect, `ControlType`, and for non-secure elements the children.
//! 3. `Name` as the label (never for the window root, a TitleBar or a Document: page titles; Tab,
//!    TabItem and TitleBar subtrees, address-bar-like Edit controls, bookmark bars and download
//!    shelves are dropped by the shared walker, S2-5).
//! 4. Editable (Edit/ComboBox) elements: `ValuePattern.CurrentValue` is fetched ONLY to test
//!    for emptiness and dropped immediately (UIA offers no "is empty" query); its text is never
//!    stored. Static text (`Text` control type) is emitted through its `Name`.
//!
//! Coordinates: physical virtual-desktop pixels; the thread must be per-monitor DPI aware
//! (`ensure_per_monitor_dpi`, fails closed). App id: lowercase exe file name of the window's
//! process (same resolution as `capture::windows_wgc`, so deny-list matching lines up).
//!
//! Chromium/Electron build their UIA tree lazily after a UIA client queries them; if the first
//! walk finds only the root, the walk is repeated (100 ms apart, inside the 1200 ms budget). A
//! root-only result is returned as is (the firewall then reports `IncompleteTree`).

use std::time::{Duration, Instant};

use frame::Rect;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationValuePattern, TreeScope_Children, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::role_map::{uia_is_editable, uia_role};
use super::walk::{walk_tree, Limits, TreeCursor};
use super::windows::{bounding_rect, ensure_per_monitor_dpi, is_password};
use crate::{FirewallError, NodeRole, RawWindowTree, ScreenTextSource};

const RETRY_STEP: Duration = Duration::from_millis(100);
const RETRIES: u32 = 5;

/// Also wrapped by the agent cursor in `windows_agent.rs` (adds `identity`).
pub(super) struct UiaCursor {
    all: IUIAutomationCondition,
}

impl UiaCursor {
    pub(super) fn new(all: IUIAutomationCondition) -> Self {
        Self { all }
    }
}

fn control_type(n: &IUIAutomationElement) -> Result<i32, FirewallError> {
    // SAFETY: COM call on a valid element.
    unsafe { n.CurrentControlType() }
        .map(|t| t.0)
        .map_err(|_| FirewallError::Detection)
}

impl TreeCursor for UiaCursor {
    type Node = IUIAutomationElement;

    fn is_secure(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        is_password(n)
    }
    fn rect(&mut self, n: &Self::Node) -> Result<Option<Rect>, FirewallError> {
        bounding_rect(n).map(Some)
    }
    fn role(&mut self, n: &Self::Node) -> Result<NodeRole, FirewallError> {
        Ok(uia_role(control_type(n)?))
    }
    fn is_editable(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        Ok(uia_is_editable(control_type(n)?))
    }
    fn has_value(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        // Emptiness only; the BSTR is dropped at the end of this function. If ValuePattern is
        // unavailable or CurrentValue fails, the field state is UNKNOWN and is reported as
        // empty (`[empty]`): privacy-safe (never invents or leaks text), but an accuracy limit
        // (see docs/manual-tests.md). Failing the whole read here would make most trees
        // unreadable, so this is deliberately neutral rather than an error.
        // SAFETY: COM calls on a valid element.
        unsafe {
            let Ok(p) = n.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            else {
                return Ok(false);
            };
            match p.CurrentValue() {
                Ok(v) => Ok(!v.is_empty()),
                Err(_) => Ok(false),
            }
        }
    }
    fn label(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError> {
        // SAFETY: COM call on a valid element.
        let name = unsafe { n.CurrentName() }.map_err(|_| FirewallError::Detection)?;
        let s = name.to_string();
        Ok((!s.trim().is_empty()).then_some(s))
    }
    fn text_value(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError> {
        // Static text carries its content in Name.
        self.label(n)
    }
    fn children(&mut self, n: &Self::Node) -> Result<Vec<Self::Node>, FirewallError> {
        // SAFETY: COM calls on a valid element and condition.
        unsafe {
            let found = n
                .FindAll(TreeScope_Children, &self.all)
                .map_err(|_| FirewallError::Detection)?;
            let len = found.Length().map_err(|_| FirewallError::Detection)?;
            let mut out = Vec::with_capacity(len.max(0) as usize);
            for i in 0..len {
                out.push(found.GetElement(i).map_err(|_| FirewallError::Detection)?);
            }
            Ok(out)
        }
    }
}

/// Lowercase exe file name of `pid`; `None` if it cannot be resolved.
fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: handle closed right away; buffer length passed with the pointer.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16(&buf[..len as usize]).ok()?;
        path.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
    }
}

fn foreground() -> Result<(HWND, u32), FirewallError> {
    // SAFETY: plain Win32 getters; pid out pointer is valid.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Err(FirewallError::Detection);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return Err(FirewallError::Detection);
        }
        Ok((hwnd, pid))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WinTextSource;

impl ScreenTextSource for WinTextSource {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError> {
        ensure_per_monitor_dpi()?;
        let limits = Limits::standard(Instant::now());
        let (hwnd, pid) = foreground()?;
        // SAFETY: no arguments.
        if pid == unsafe { GetCurrentProcessId() } {
            // Our own overlay/panel: voice-only.
            return Err(FirewallError::IncompleteTree { app: String::new() });
        }
        let app_id = exe_name(pid).unwrap_or_default();

        // SAFETY: COM on this thread; every HRESULT is checked and mapped to a fixed error.
        let mut cursor = unsafe {
            // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is already usable on this thread.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|_| FirewallError::Detection)?;
            let all = auto
                .CreateTrueCondition()
                .map_err(|_| FirewallError::Detection)?;
            (UiaCursor { all }, auto)
        };
        let read = |c: &mut UiaCursor, auto: &IUIAutomation| {
            // SAFETY: COM call with the foreground HWND just obtained.
            let root =
                unsafe { auto.ElementFromHandle(hwnd) }.map_err(|_| FirewallError::Detection)?;
            walk_tree(c, root, app_id.clone(), &limits)
        };

        let mut tree = read(&mut cursor.0, &cursor.1)?;
        // Lazily built Chromium/Electron trees: querying triggers the build; retry while root-only.
        for _ in 0..RETRIES {
            if tree.nodes.len() > 1 || Instant::now() + RETRY_STEP >= limits.deadline {
                break;
            }
            std::thread::sleep(RETRY_STEP);
            tree = read(&mut cursor.0, &cursor.1)?;
        }
        // The foreground window must be the one we read.
        match foreground() {
            Ok((h, p)) if h == hwnd && p == pid => Ok(tree),
            _ => Err(FirewallError::Detection),
        }
    }
}
