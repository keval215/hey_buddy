//! Windows actuator (S2-14d2, ADR 0004 sections 3, 7, 9, 10): UI Automation PATTERNS (never a
//! synthetic mouse), keyboard input, `CreateProcessW`. UNTESTED on this machine (compile-checked
//! for `x86_64-pc-windows-gnu` only); every decision that can be pure lives in `input_plan.rs`
//! and is unit-tested on Linux. Windows behaviour to verify is listed in `docs/manual-tests.md`,
//! "Windows actuator (S2-14d2)".
//!
//! * Reachability: [`WinActuator`] is `pub(crate)`. The only way out of this crate is
//!   [`windows_driver`], which returns a `Box<dyn DesktopDriver>`: the actuator can only be driven
//!   by `Driver::execute` (sealed `ApprovedAction`, kill switch, firewall revalidation).
//! * No reads. Not one UIA content property is read here; the only `Current*` call is
//!   `CurrentIsReadOnly` before `SetValue` (CI read-ban grep). The element acted on is the one the
//!   firewall re-resolved and checked (`LiveTarget::uia_element`); this file never searches.
//! * No mouse, no coordinates, no window messages, no shell: keyboard events through a single
//!   batch call per chunk, UIA patterns, `SetForegroundWindow`, `CreateProcessW`.
//! * Threads: this actuator spawns none. The driver runs every call on its worker, which calls
//!   `init_thread` (COM, tolerant of an already initialised thread) first.
//! * Typed text exists only inside the `send_chars` argument and the event list built from it; it
//!   is never logged and no error carries it.

use std::mem::size_of;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use action::{KeyCombo, ScrollAmount, ScrollDirection, TypedText};
use firewall::{AppId, Firewall, LiveTarget};
use policy::Clock;
use windows::core::{Interface, BOOL, BSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenIntegrityLevel, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
    IUIAutomationScrollPattern, IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    IUIAutomationValuePattern, ScrollAmount_LargeDecrement, ScrollAmount_LargeIncrement,
    ScrollAmount_NoAmount, ScrollAmount_SmallDecrement, ScrollAmount_SmallIncrement,
    UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_ScrollPatternId,
    UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ValuePatternId, UIA_PATTERN_ID,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetForegroundWindow, GW_OWNER,
};

use crate::actuator::{Actuator, ClickMethod};
use crate::driver::{DesktopDriver, Driver};
use crate::error::{ActError, DriverError};
use crate::input_plan::{
    char_events, chord, chord_events, copy_chord, drain_stale_with, integrity_allows, launch_error,
    quoted_command_line, release_all_with, scroll_plan, send_batch_with, uia_error,
    win32_from_hresult, window_qualifies, Held, InputEvent, PressedSet, ScrollStep,
};
use crate::limits::LaunchAllowlist;

/// Upper bound of top-level windows inspected by `focus_app`.
const MAX_WINDOWS: usize = 1024;
/// How long `focus_app` waits for the foreground window to become the one it asked for.
const FOCUS_WAIT: Duration = Duration::from_millis(250);
const FOCUS_POLL: Duration = Duration::from_millis(25);

/// The Windows effector. `pub(crate)`: no other crate can name or hold it.
pub(crate) struct WinActuator {
    /// Every key / Unicode unit this actuator pressed and did not yet release.
    pressed: Mutex<PressedSet>,
}

/// The ONLY way to get a Windows desktop driver. Refuses to start when this process is elevated
/// (`Unsupported("elevated")`: an elevated hey_buddy could act on anything) or when the token
/// cannot be queried (fail closed).
pub fn windows_driver(
    firewall: Arc<dyn Firewall>,
    launch: LaunchAllowlist,
    clock: Arc<dyn Clock>,
) -> Result<Box<dyn DesktopDriver>, DriverError> {
    match process_is_elevated() {
        Ok(false) => {}
        Ok(true) => return Err(DriverError::Unsupported("elevated")),
        Err(()) => return Err(DriverError::Platform("token query failed")),
    }
    Ok(Box::new(
        Driver::new(firewall, WinActuator::new())
            .with_launch_allowlist(launch)
            .with_clock(clock),
    ))
}

fn process_is_elevated() -> Result<bool, ()> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; `token` is a valid out pointer and is closed
    // below on every path after a successful open.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|_| ())?;
    let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut ret = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION and its size is passed with the pointer.
    let r = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elev as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
    };
    // SAFETY: we opened `token` above.
    unsafe {
        let _ = CloseHandle(token);
    }
    r.map_err(|_| ())?;
    Ok(elev.TokenIsElevated != 0)
}

fn hr(e: windows::core::Error) -> ActError {
    uia_error(e.code().0)
}

/// The element the firewall re-resolved for this target; none means fail closed.
fn element<'a>(t: &'a LiveTarget<'_>) -> Result<&'a IUIAutomationElement, ActError> {
    t.uia_element().ok_or(ActError::Unsupported("no element"))
}

/// A UIA pattern of `el`; a missing pattern is `Unsupported` (nothing was sent).
fn pattern<T: Interface>(el: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Result<T, ActError> {
    // SAFETY: COM call on a valid element; failure (pattern not offered) maps to Unsupported.
    unsafe { el.GetCurrentPatternAs::<T>(id) }
        .map_err(|_| ActError::Unsupported("pattern not supported"))
}

fn scroll_amount(s: ScrollStep) -> windows::Win32::UI::Accessibility::ScrollAmount {
    match s {
        ScrollStep::NoAmount => ScrollAmount_NoAmount,
        ScrollStep::SmallDecrement => ScrollAmount_SmallDecrement,
        ScrollStep::LargeDecrement => ScrollAmount_LargeDecrement,
        ScrollStep::SmallIncrement => ScrollAmount_SmallIncrement,
        ScrollStep::LargeIncrement => ScrollAmount_LargeIncrement,
    }
}

/// Plain event data -> the OS struct (the only place that happens).
fn to_input(e: &InputEvent) -> INPUT {
    let up = if e.up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    let ki = match e.held {
        Held::Unicode(unit) => KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: unit,
            dwFlags: KEYEVENTF_UNICODE | up,
            time: 0,
            dwExtraInfo: 0,
        },
        Held::Key(k) => KEYBDINPUT {
            wVk: VIRTUAL_KEY(k.vk),
            wScan: k.scan,
            dwFlags: if k.extended {
                KEYEVENTF_EXTENDEDKEY | up
            } else {
                up
            },
            time: 0,
            dwExtraInfo: 0,
        },
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    }
}

/// One batch call. Returns how many events the OS accepted and, if fewer than requested, the
/// Win32 error. The return value is the ONLY thing known; it is never delivery proof.
fn submit(events: &[InputEvent]) -> (usize, u32) {
    let inputs: Vec<INPUT> = events.iter().map(to_input).collect();
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs and the size passed is
    // that of one INPUT. Keyboard events only.
    let n = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    let err = if n == events.len() {
        0
    } else {
        // SAFETY: plain getter, read right after the failing call.
        unsafe { GetLastError() }.0
    };
    (n, err)
}

impl WinActuator {
    pub(crate) fn new() -> Self {
        Self {
            pressed: Mutex::new(PressedSet::default()),
        }
    }

    /// Releases keys left over from an earlier double release failure. Called before any new
    /// input (init, every primitive): the driver runs one worker at a time, so nothing else is
    /// pressing keys.
    fn drain_stale(&self) {
        drain_stale_with(&self.pressed, &mut submit);
    }

    /// Sends `events` as ONE batch. Keys count as pressed from before the call until the OS
    /// reports them released, so `release_all` (kill path, other thread) covers a call in flight.
    /// A count mismatch is an error; whatever is still down is released at once, retried once in
    /// this call, and otherwise stays remembered (the next call drains it). All logic, and its
    /// tests, are in `input_plan::send_batch_with`.
    fn send_events(&self, events: &[InputEvent]) -> Result<(), ActError> {
        send_batch_with(&self.pressed, events, &mut submit)
    }

    /// The top-z visible, un-cloaked, un-minimised, unowned top-level window of `app`.
    fn top_window_of(app: &AppId) -> Option<HWND> {
        let mut handles: Vec<isize> = Vec::new();
        // SAFETY: the callback only touches the Vec behind the pointer, which outlives the call.
        unsafe {
            EnumWindows(
                Some(collect_window),
                LPARAM((&mut handles as *mut Vec<isize>) as isize),
            )
        }
        .ok()?;
        // SAFETY: plain getter.
        let own_pid = unsafe { GetCurrentProcessId() };
        for raw in handles {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            let mut pid = 0u32;
            // SAFETY: plain Win32 getters on a handle that may have died meanwhile (then they fail).
            let (visible, minimized, owned) = unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                (
                    IsWindowVisible(hwnd).as_bool(),
                    IsIconic(hwnd).as_bool(),
                    GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()),
                )
            };
            if !window_qualifies(visible, is_cloaked(hwnd), minimized, owned, pid, own_pid) {
                continue;
            }
            if app_id_of_pid(pid).as_ref() == Some(app)
                && integrity_allows(process_integrity_rid(pid), own_integrity_rid())
            {
                return Some(hwnd);
            }
        }
        None
    }
}

/// `EnumWindows` callback: collect handles (bounded) into the `Vec<isize>` behind `lparam`.
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the address of a live `Vec<isize>` owned by `top_window_of`.
    let v = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    if v.len() < MAX_WINDOWS {
        v.push(hwnd.0 as isize);
    }
    BOOL(1)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: valid out pointer of the stated size. A failed query counts as cloaked.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .map_or(true, |()| cloaked != 0)
}

/// Mandatory-integrity RID of a token. `None` when it cannot be read (including the access-denied
/// a higher-integrity process gives): the caller then skips the window.
fn token_integrity_rid(process: HANDLE) -> Option<u32> {
    let mut token = HANDLE::default();
    // SAFETY: valid process handle and out pointer; the token is closed below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let mut needed = 0u32;
    // SAFETY: size query with a null buffer; the expected failure only fills `needed`.
    let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed) };
    let rid = if needed == 0 || needed > 4096 {
        None
    } else {
        // 8-byte aligned buffer for the pointer-containing TOKEN_MANDATORY_LABEL.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: the buffer is at least `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(buf.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
        }
        .is_ok();
        if ok {
            // SAFETY: filled TOKEN_MANDATORY_LABEL; its SID lives in the same buffer and the
            // sub-authority pointers are null-checked before use.
            unsafe {
                let label = &*(buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
                let sid = label.Label.Sid;
                let count = GetSidSubAuthorityCount(sid);
                if count.is_null() || *count == 0 {
                    None
                } else {
                    let r = GetSidSubAuthority(sid, u32::from(*count) - 1);
                    (!r.is_null()).then(|| *r)
                }
            }
        } else {
            None
        }
    };
    // SAFETY: we opened `token` above.
    unsafe {
        let _ = CloseHandle(token);
    }
    rid
}

fn own_integrity_rid() -> Option<u32> {
    // SAFETY: the pseudo handle needs no closing.
    token_integrity_rid(unsafe { GetCurrentProcess() })
}

/// Integrity RID of another process; `None` when it cannot be opened or read.
fn process_integrity_rid(pid: u32) -> Option<u32> {
    // SAFETY: plain Win32 call; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let rid = token_integrity_rid(h);
    // SAFETY: we opened `h` above.
    unsafe {
        let _ = CloseHandle(h);
    }
    rid
}

/// Lowercase exe name of a process as an [`AppId`]; `None` when its image path cannot be read.
/// (A higher-integrity process is not a name problem: `focus_app` skips it via
/// `integrity_allows`, which also skips a process whose integrity is unknown.)
fn app_id_of_pid(pid: u32) -> Option<AppId> {
    // SAFETY: plain Win32 call; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 2048];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and its length are passed together; the handle is valid.
    let r = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    };
    // SAFETY: we opened `h` above.
    unsafe {
        let _ = CloseHandle(h);
    }
    r.ok()?;
    let path = String::from_utf16(buf.get(..len as usize)?).ok()?;
    let base = path.rsplit(['\\', '/']).next().filter(|s| !s.is_empty())?;
    AppId::parse(base)
}

impl Actuator for WinActuator {
    fn init_thread(&self) {
        // SAFETY: COM on this (worker) thread. S_FALSE / RPC_E_CHANGED_MODE just mean COM is
        // already usable here, so the result is deliberately ignored. The MTA on the worker keeps
        // the firewall's AgileReference valid even after the firewall deadline thread has exited.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        self.drain_stale();
    }

    fn invoke(&self, t: &LiveTarget<'_>, m: ClickMethod) -> Result<(), ActError> {
        self.drain_stale();
        let el = element(t)?;
        // SAFETY (all arms): COM calls on a pattern of the element the firewall checked.
        match m {
            ClickMethod::Invoke => {
                let p: IUIAutomationInvokePattern = pattern(el, UIA_InvokePatternId)?;
                unsafe { p.Invoke() }.map_err(hr)
            }
            ClickMethod::Toggle => {
                let p: IUIAutomationTogglePattern = pattern(el, UIA_TogglePatternId)?;
                unsafe { p.Toggle() }.map_err(hr)
            }
            ClickMethod::Select => {
                let p: IUIAutomationSelectionItemPattern = pattern(el, UIA_SelectionItemPatternId)?;
                unsafe { p.Select() }.map_err(hr)
            }
            // The state is not read (read-ban): a combo box is always asked to open.
            ClickMethod::ExpandCollapse => {
                let p: IUIAutomationExpandCollapsePattern =
                    pattern(el, UIA_ExpandCollapsePatternId)?;
                unsafe { p.Expand() }.map_err(hr)
            }
        }
    }

    fn set_value(&self, t: &LiveTarget<'_>, text: &TypedText) -> Result<(), ActError> {
        self.drain_stale();
        let el = element(t)?;
        let p: IUIAutomationValuePattern = pattern(el, UIA_ValuePatternId)?;
        // The ONLY `Current*` read allowed in this crate: a read-only element is refused.
        // SAFETY: COM call; an unreadable flag is a refusal (fail closed).
        match unsafe { p.CurrentIsReadOnly() } {
            Ok(ro) if !ro.as_bool() => {}
            Ok(_) => return Err(ActError::Platform("element is read-only")),
            Err(e) => return Err(hr(e)),
        }
        let value = BSTR::from(text.as_str());
        // SAFETY: COM call with a valid BSTR that lives across the call.
        unsafe { p.SetValue(&value) }.map_err(hr)
    }

    fn set_focus(&self, t: &LiveTarget<'_>) -> Result<(), ActError> {
        self.drain_stale();
        let el = element(t)?;
        // SAFETY: COM call on the element the firewall checked.
        unsafe { el.SetFocus() }.map_err(hr)
    }

    fn scroll(
        &self,
        t: &LiveTarget<'_>,
        d: ScrollDirection,
        a: ScrollAmount,
    ) -> Result<(), ActError> {
        self.drain_stale();
        let el = element(t)?;
        let p: IUIAutomationScrollPattern = pattern(el, UIA_ScrollPatternId)?;
        let (h, v) = scroll_plan(d, a);
        // SAFETY: COM call; no scrollability property is read (a direction that cannot scroll is
        // reported by the call itself).
        unsafe { p.Scroll(scroll_amount(h), scroll_amount(v)) }.map_err(hr)
    }

    fn send_chars(&self, chunk: &str) -> Result<(), ActError> {
        self.drain_stale();
        let events = char_events(chunk)?;
        self.send_events(&events)
    }

    fn send_key(&self, k: KeyCombo) -> Result<(), ActError> {
        self.drain_stale();
        self.send_events(&chord_events(&chord(k)))
    }

    /// Ctrl+C keys only. The clipboard is never opened or read.
    fn send_copy(&self) -> Result<(), ActError> {
        self.drain_stale();
        self.send_events(&chord_events(&copy_chord()))
    }

    fn launch(&self, path: &Path) -> Result<(), ActError> {
        self.drain_stale();
        let s = path
            .to_str()
            .ok_or(ActError::Platform("launch path invalid"))?;
        // `"<path>"`, no arguments; also re-validates (absolute local .exe, no `..`, no UNC).
        let cmd = quoted_command_line(s).ok_or(ActError::Platform("launch path invalid"))?;
        // A missing file is a clean refusal; there is never a PATH search.
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => {}
            _ => return Err(ActError::Platform("launch target missing")),
        }
        let app: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        // CreateProcessW may modify the command line buffer, so it must be mutable.
        let mut cmdline: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: both buffers are NUL-terminated and outlive the call; handles are NOT inherited;
        // no environment or directory override; flags are 0 (GUI apps get their window, and no
        // CREATE_NO_WINDOW); the out structs are valid. Never ShellExecute: no verbs, no URIs.
        let r = unsafe {
            CreateProcessW(
                windows::core::PCWSTR(app.as_ptr()),
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                windows::core::PCWSTR::null(),
                &si,
                &mut pi,
            )
        };
        match r {
            Ok(()) => {
                // SAFETY: both handles were just returned to us and are closed exactly once.
                unsafe {
                    let _ = CloseHandle(pi.hProcess);
                    let _ = CloseHandle(pi.hThread);
                }
                Ok(())
            }
            // ERROR_ELEVATION_REQUIRED is a refusal here, never a UAC prompt.
            Err(e) => Err(launch_error(win32_from_hresult(e.code().0))),
        }
    }

    fn focus_app(&self, app: &AppId) -> Result<(), ActError> {
        self.drain_stale();
        let hwnd = Self::top_window_of(app).ok_or(ActError::Platform("no window to focus"))?;
        // SAFETY: plain Win32 call on a handle that may have died meanwhile (then it fails).
        // The foreground lock may refuse; that is reported, never worked around (no input
        // injection, no AttachThreadInput).
        if !unsafe { SetForegroundWindow(hwnd) }.as_bool() {
            return Err(ActError::Platform("focus refused"));
        }
        let end = Instant::now() + FOCUS_WAIT;
        loop {
            // SAFETY: plain getter.
            if unsafe { GetForegroundWindow() } == hwnd {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(ActError::Platform("focus not taken"));
            }
            std::thread::sleep(FOCUS_POLL);
        }
    }

    /// Lets go of every key this actuator pressed and did not release. Idempotent; safe from the
    /// kill path while an input call is in flight (the lock is never held across the OS call).
    fn release_all(&self) {
        let _ = release_all_with(&self.pressed, &mut submit);
    }
}
