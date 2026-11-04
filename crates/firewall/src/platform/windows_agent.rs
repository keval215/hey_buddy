//! Windows agent observation (S2-14d1, ADR 0004 §5, §6, §8, §12, §19): `ObservationSource` and
//! `TargetResolver` over UI Automation. UNTESTED on this machine (compile-checked only,
//! `x86_64-pc-windows-gnu`); every decision that can be pure lives in `win_pure.rs` and is
//! unit-tested on Linux, and the tree walk is the shared `walk_tree_identified`.
//!
//! # Observation
//! Foreground window only (`GetForegroundWindow` -> `ElementFromHandle`), walked by the same
//! secure-first walker as the text source. `TreeCursor::identity` (below) runs ONLY after
//! `is_secure` (`IsPassword`) returned false and only for nodes that will be emitted: runtime id
//! (SAFEARRAY -> `Vec<i32>`), control type and the SHA-256 of the AutomationId (the raw string is
//! dropped inside the cursor). It never reads the Name: the walker hashes the label it already
//! read. Window identity: hwnd, pid, process creation time (`GetProcessTimes`), image path
//! (`QueryFullProcessImageNameW`). The visible-apps list is `EnumWindows` (visible, not cloaked,
//! not minimised, not ours), app ids only.
//!
//! # Revalidation
//! One element is re-resolved INSIDE the observed hwnd only (no global search, never
//! `ElementFromPoint`). Chosen approach: `ElementFromHandle(hwnd)` then `FindFirst(Descendants)`
//! with a `UIA_RuntimeIdPropertyId` property condition (a `VT_ARRAY | VT_I4` SAFEARRAY);
//! the hit must have exactly the requested runtime id. FALLBACK (property condition missing or
//! unreliable, ADR 0004 section 19 VERIFY): a bounded, time-boxed subtree walk from the same
//! root that asks `IsPassword` of every node first, never descends into a password field, and
//! compares runtime ids. Neither path guesses: no match is `Gone`; running out of budget is `Err`.
//!
//! The re-resolved element is read in this order only: `IsPassword`; if false, control type,
//! AutomationId (hashed), Name (hashed immediately, never returned or logged), bounding rect.
//!
//! Focus: `GetFocusedElement`. `IsPassword` of the focused element is read first, then (text-like
//! controls only) its Name is checked against the secret lexicon and dropped
//! (`FocusState::Sensitive`); then `CompareElements(focused, target)` and a bounded walk up the
//! raw-view ancestors for "inside the target".
//!
//! All UIA/COM calls run on the firewall's deadline workers; each call initialises COM on its
//! thread (same pattern as `windows_text.rs`). Coordinates are physical pixels and the thread must
//! be per-monitor DPI aware (`ensure_per_monitor_dpi`, fail closed).
//!
//! UNTESTED (all of it; see `docs/manual-tests.md`, "Windows agent observation (S2-14d1)").

use std::collections::HashMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use frame::Rect;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, FILETIME, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayCreateVector, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement,
    SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype, SafeArrayPutElement,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_ACCESS_FLAGS,
    DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, UOI_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VARENUM, VARIANT, VT_ARRAY, VT_I4};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationScrollPattern, IUIAutomationTreeWalker, TreeScope_Children, TreeScope_Descendants,
    UIA_IsScrollPatternAvailablePropertyId, UIA_RuntimeIdPropertyId, UIA_ScrollPatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

use super::walk::{walk_tree_identified, Limits, TreeCursor, MAX_DEPTH, MAX_NODES};
use super::win_pure::{
    app_id_from_exe_path, filetime_ticks, include_in_apps, is_user_desktop_name, node_identity,
    place_focus, read_int32_array, read_resolved, rect_from_edges, runtime_ids_equal, same_process,
    ElementReads, FocusCursor, Int32Array, IntegrityLevel, MAX_RUNTIME_ID_LEN,
};
use super::windows::{ensure_per_monitor_dpi, is_password};
use super::windows_text::UiaCursor;
use crate::target::{
    ElementHandle, FocusState, ForegroundFacts, NodeIdentity, ObservationSource, ObservedWindow,
    ResolveRequest, ResolvedTarget, TargetResolver, VisibleApp, WindowFacts,
};
use crate::{FirewallError, NodeRole, RawWindowTree};

/// Same Chromium/Electron lazy-tree retry as the text source.
const RETRY_STEP: Duration = Duration::from_millis(100);
const RETRIES: u32 = 5;
/// Budget of the fallback subtree walk inside `resolve` (the resolver deadline is 1000 ms).
const FALLBACK_WALK_BUDGET: Duration = Duration::from_millis(700);
/// Upper bound of top-level windows inspected for the `apps:` list.
const MAX_ENUMERATED_WINDOWS: usize = 1024;

fn fail<E>(_: E) -> FirewallError {
    FirewallError::Detection
}

/// Initialise COM on this (worker) thread and create the automation object.
fn automation() -> Result<IUIAutomation, FirewallError> {
    // SAFETY: COM on this thread; every HRESULT is checked and mapped to a fixed error.
    unsafe {
        // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is already usable on this thread.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(fail)
    }
}

// ---------- processes and windows ----------

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open_process(pid: u32) -> Result<OwnedHandle, FirewallError> {
    // SAFETY: plain Win32 call; the handle is owned by the returned guard.
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map(OwnedHandle)
        .map_err(fail)
}

fn creation_time(h: &OwnedHandle) -> Result<u64, FirewallError> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: valid handle and four valid out pointers.
    unsafe { GetProcessTimes(h.0, &mut created, &mut exited, &mut kernel, &mut user) }
        .map_err(fail)?;
    match filetime_ticks(created.dwLowDateTime, created.dwHighDateTime) {
        0 => Err(FirewallError::Detection),
        t => Ok(t),
    }
}

fn image_path(h: &OwnedHandle) -> Result<String, FirewallError> {
    let mut buf = vec![0u16; 2048];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and its length are passed together; the handle is valid.
    unsafe {
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .map_err(fail)?;
    String::from_utf16(buf.get(..len as usize).ok_or(FirewallError::Detection)?).map_err(fail)
}

/// Mandatory integrity level of the process token. Opening a HIGHER-integrity process's token
/// from a normal one is denied: `denied_means_higher` maps that (and only that) error to the most
/// privileged level so the firewall reports `ElevatedTarget` instead of a generic block. For our
/// own process there is no such leniency.
fn integrity_of(h: HANDLE, denied_means_higher: bool) -> Result<IntegrityLevel, FirewallError> {
    let mut token = HANDLE::default();
    // SAFETY: valid process handle and out pointer.
    if let Err(e) = unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) } {
        return if denied_means_higher && e.code() == E_ACCESSDENIED {
            Ok(IntegrityLevel::Protected)
        } else {
            Err(FirewallError::Detection)
        };
    }
    let token = OwnedHandle(token);
    let mut needed = 0u32;
    // SAFETY: size query with a null buffer; failure with ERROR_INSUFFICIENT_BUFFER is expected,
    // so only `needed` matters.
    let _ = unsafe { GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut needed) };
    if needed == 0 || needed > 4096 {
        return Err(FirewallError::Detection);
    }
    // 8-byte aligned buffer for the pointer-containing TOKEN_MANDATORY_LABEL.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `needed` bytes.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    }
    .map_err(fail)?;
    // SAFETY: GetTokenInformation filled a TOKEN_MANDATORY_LABEL whose SID lives in the same
    // buffer; the sub-authority pointers are checked for null before use.
    unsafe {
        let label = &*(buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
        let sid = label.Label.Sid;
        let count = GetSidSubAuthorityCount(sid);
        if count.is_null() || *count == 0 {
            return Err(FirewallError::Detection);
        }
        let rid = GetSidSubAuthority(sid, u32::from(*count) - 1);
        if rid.is_null() {
            return Err(FirewallError::Detection);
        }
        Ok(IntegrityLevel::from_rid(*rid))
    }
}

fn own_integrity() -> Result<IntegrityLevel, FirewallError> {
    // SAFETY: the pseudo handle needs no closing.
    integrity_of(unsafe { GetCurrentProcess() }, false)
}

fn hwnd_to_u64(h: HWND) -> u64 {
    h.0 as usize as u64
}

fn hwnd_from_u64(h: u64) -> HWND {
    HWND(h as usize as *mut c_void)
}

fn foreground() -> Result<(HWND, u32), FirewallError> {
    // SAFETY: plain Win32 getters; the pid out pointer is valid.
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

/// Window identity of `hwnd` owned by `pid`, the app id (lowercase exe name) and the process
/// handle (for the integrity query).
fn window_identity(
    hwnd: HWND,
    pid: u32,
) -> Result<(WindowFacts, String, OwnedHandle), FirewallError> {
    let h = open_process(pid)?;
    let exe_path = image_path(&h)?;
    let app_id = app_id_from_exe_path(&exe_path).ok_or(FirewallError::Detection)?;
    let facts = WindowFacts {
        hwnd: hwnd_to_u64(hwnd),
        pid,
        process_creation_time: creation_time(&h)?,
        exe_path,
    };
    Ok((facts, app_id, h))
}

/// The input desktop is the user's normal one: `OpenInputDesktop` succeeds AND its name is
/// "Default" (`UOI_NAME`). Any failure is "not the user desktop" (fail closed).
fn input_desktop_is_user_desktop() -> bool {
    // SAFETY: handle closed before returning; the buffer length is passed with the pointer.
    unsafe {
        let access = DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS.0 | DESKTOP_SWITCHDESKTOP.0);
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access) else {
            return false;
        };
        let mut buf = [0u16; 128];
        let mut needed = 0u32;
        let r = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            std::mem::size_of_val(&buf) as u32,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desk);
        r.is_ok() && is_user_desktop_name(&buf)
    }
}

/// `EnumWindows` callback: collect handles (bounded) into the `Vec<isize>` behind `lparam`.
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the address of a live `Vec<isize>` owned by `visible_apps`.
    let v = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    if v.len() < MAX_ENUMERATED_WINDOWS {
        v.push(hwnd.0 as isize);
    }
    BOOL(1)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: valid out pointer of the stated size. A failed query counts as cloaked (not listed).
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .map_or(true, |()| cloaked != 0)
}

/// Visible top-level apps (app id and pid only). A window whose process cannot be resolved is
/// simply not listed. Deny / auth filtering is done by the firewall core.
fn visible_apps(own_pid: u32) -> Result<Vec<VisibleApp>, FirewallError> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback only touches the Vec behind the pointer, which outlives the call.
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM((&mut handles as *mut Vec<isize>) as isize),
        )
    }
    .map_err(fail)?;
    let mut by_pid: HashMap<u32, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for raw in handles {
        let hwnd = HWND(raw as *mut c_void);
        // SAFETY: plain Win32 getters on a handle that may have died meanwhile (then they fail).
        let (visible, minimized, pid) = unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            (
                IsWindowVisible(hwnd).as_bool(),
                IsIconic(hwnd).as_bool(),
                pid,
            )
        };
        if !include_in_apps(visible, is_cloaked(hwnd), minimized, pid, own_pid) {
            continue;
        }
        let app = by_pid.entry(pid).or_insert_with(|| {
            open_process(pid)
                .and_then(|h| image_path(&h))
                .ok()
                .and_then(|p| app_id_from_exe_path(&p))
        });
        if let Some(app_id) = app.clone() {
            out.push(VisibleApp { app_id, pid });
        }
    }
    Ok(out)
}

// ---------- runtime ids ----------

/// A `SAFEARRAY` returned by UIA (`GetRuntimeId`); destroyed on drop.
struct SafeArrayI32(*mut SAFEARRAY);

impl Int32Array for SafeArrayI32 {
    fn lower_bound(&self) -> Option<i32> {
        // SAFETY: non-null array owned by `self`.
        unsafe {
            if SafeArrayGetDim(self.0) != 1 {
                return None;
            }
            SafeArrayGetLBound(self.0, 1).ok()
        }
    }
    fn upper_bound(&self) -> Option<i32> {
        // SAFETY: non-null array owned by `self`.
        unsafe {
            if SafeArrayGetDim(self.0) != 1 {
                return None;
            }
            SafeArrayGetUBound(self.0, 1).ok()
        }
    }
    fn get(&self, index: i32) -> Option<i32> {
        let mut v = 0i32;
        // SAFETY: the array holds VT_I4 (checked in `runtime_id`); `v` is a valid 4-byte target.
        unsafe { SafeArrayGetElement(self.0, &index, (&mut v as *mut i32).cast::<c_void>()) }
            .ok()?;
        Some(v)
    }
}

impl Drop for SafeArrayI32 {
    fn drop(&mut self) {
        // SAFETY: we own the array UIA handed out.
        unsafe {
            let _ = SafeArrayDestroy(self.0);
        }
    }
}

/// UIA runtime id of `el` as integers. Identity only; no content.
fn runtime_id(el: &IUIAutomationElement) -> Result<Vec<i32>, FirewallError> {
    // SAFETY: COM call on a valid element; the returned array is owned by the guard.
    let psa = unsafe { el.GetRuntimeId() }.map_err(fail)?;
    if psa.is_null() {
        return Err(FirewallError::Detection);
    }
    let arr = SafeArrayI32(psa);
    // SAFETY: non-null array.
    match unsafe { SafeArrayGetVartype(arr.0) } {
        Ok(vt) if vt == VT_I4 => {}
        _ => return Err(FirewallError::Detection),
    }
    read_int32_array(&arr).ok_or(FirewallError::Detection)
}

/// `VT_ARRAY | VT_I4` variant holding `rid` (for the `UIA_RuntimeIdPropertyId` condition). The
/// array is owned by the variant (`VariantClear` destroys it on drop).
fn runtime_id_variant(rid: &[i32]) -> Option<VARIANT> {
    // Check the length UP FRONT so no conversion can fail after the array exists (a `?` between
    // creation and the variant would leak the SAFEARRAY).
    if rid.is_empty() || rid.len() > MAX_RUNTIME_ID_LEN {
        return None;
    }
    let len = u32::try_from(rid.len()).ok()?;
    // SAFETY: the array is created, filled and then moved into the variant; on any failure it is
    // destroyed here. Writing the union fields of a freshly built VT_I4 variant is sound because
    // it owns no resources.
    unsafe {
        let psa = SafeArrayCreateVector(VT_I4, 0, len);
        if psa.is_null() {
            return None;
        }
        for (i, v) in rid.iter().enumerate() {
            // `i < rid.len() <= MAX_RUNTIME_ID_LEN`: always fits.
            let idx = i as i32;
            if SafeArrayPutElement(psa, &idx, (v as *const i32).cast::<c_void>()).is_err() {
                let _ = SafeArrayDestroy(psa);
                return None;
            }
        }
        let mut var = VARIANT::from(0i32);
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VARENUM(VT_ARRAY.0 | VT_I4.0);
        inner.Anonymous.parray = psa;
        Some(var)
    }
}

// ---------- the observation cursor ----------

/// The text-source cursor plus `identity`. Every other method delegates unchanged, so the agent
/// walk reads exactly what `check_text` reads.
struct AgentCursor(UiaCursor);

fn control_type(n: &IUIAutomationElement) -> Result<i32, FirewallError> {
    // SAFETY: COM call on a valid element.
    unsafe { n.CurrentControlType() }.map(|t| t.0).map_err(fail)
}

impl TreeCursor for AgentCursor {
    type Node = IUIAutomationElement;

    fn is_secure(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        self.0.is_secure(n)
    }
    fn rect(&mut self, n: &Self::Node) -> Result<Option<Rect>, FirewallError> {
        self.0.rect(n)
    }
    fn role(&mut self, n: &Self::Node) -> Result<NodeRole, FirewallError> {
        self.0.role(n)
    }
    fn is_editable(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        self.0.is_editable(n)
    }
    fn has_value(&mut self, n: &Self::Node) -> Result<bool, FirewallError> {
        self.0.has_value(n)
    }
    fn label(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError> {
        self.0.label(n)
    }
    fn text_value(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError> {
        self.0.text_value(n)
    }
    fn children(&mut self, n: &Self::Node) -> Result<Vec<Self::Node>, FirewallError> {
        self.0.children(n)
    }

    /// Called by the walker ONLY after `is_secure` returned false for `n`, and only for nodes
    /// that will be emitted. Reads runtime id, control type and AutomationId (hashed here, the
    /// raw string is dropped). The Name is never read here.
    fn identity(&mut self, n: &Self::Node) -> Result<NodeIdentity, FirewallError> {
        let rid = runtime_id(n)?;
        let ct = control_type(n)?;
        // SAFETY: COM call on a valid element.
        let aid = unsafe { n.CurrentAutomationId() }
            .map_err(fail)?
            .to_string();
        node_identity(&rid, ct, &aid).ok_or(FirewallError::Detection)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WinObservationSource;

impl ObservationSource for WinObservationSource {
    fn observe_foreground(&self) -> Result<ObservedWindow, FirewallError> {
        ensure_per_monitor_dpi()?;
        let limits = Limits::standard(Instant::now());
        let (hwnd, pid) = foreground()?;
        // SAFETY: no arguments.
        let own_pid = unsafe { GetCurrentProcessId() };
        if pid == own_pid {
            // Our own overlay / panel is never observed.
            return Err(FirewallError::IncompleteTree { app: String::new() });
        }
        let (facts, app_id, _proc) = window_identity(hwnd, pid)?;

        let auto = automation()?;
        // SAFETY: COM call.
        let all = unsafe { auto.CreateTrueCondition() }.map_err(fail)?;
        let mut cursor = AgentCursor(UiaCursor::new(all));
        let read = |c: &mut AgentCursor| -> Result<
            (RawWindowTree, Vec<Option<NodeIdentity>>),
            FirewallError,
        > {
            // SAFETY: COM call with the foreground HWND just obtained.
            let root = unsafe { auto.ElementFromHandle(hwnd) }.map_err(fail)?;
            walk_tree_identified(c, root, app_id.clone(), &limits)
        };
        let (mut tree, mut identities) = read(&mut cursor)?;
        // Lazily built Chromium/Electron trees: querying triggers the build; retry while root-only.
        for _ in 0..RETRIES {
            if tree.nodes.len() > 1 || Instant::now() + RETRY_STEP >= limits.deadline {
                break;
            }
            std::thread::sleep(RETRY_STEP);
            (tree, identities) = read(&mut cursor)?;
        }

        // The foreground window and its process must be the ones we read (hwnd, pid, creation time).
        let (h2, p2) = foreground()?;
        let (facts2, _, _) = window_identity(h2, p2)?;
        if hwnd_to_u64(h2) != facts.hwnd
            || !same_process(
                (facts2.pid, facts2.process_creation_time),
                (facts.pid, facts.process_creation_time),
            )
            || facts2.exe_path != facts.exe_path
        {
            return Err(FirewallError::Detection);
        }
        let apps = visible_apps(own_pid)?;
        Ok(ObservedWindow {
            tree,
            identities,
            window: facts,
            visible_apps: apps,
        })
    }
}

// ---------- the resolver ----------

/// An element's reads, one attribute per call (so `read_resolved` fixes the order).
struct UiaReads<'a>(&'a IUIAutomationElement);

impl ElementReads for UiaReads<'_> {
    fn is_password(&mut self) -> Result<bool, FirewallError> {
        is_password(self.0)
    }
    fn control_type(&mut self) -> Result<i32, FirewallError> {
        control_type(self.0)
    }
    fn automation_id(&mut self) -> Result<String, FirewallError> {
        // SAFETY: COM call on a valid element; the caller hashes and drops the string.
        unsafe { self.0.CurrentAutomationId() }
            .map(|b| b.to_string())
            .map_err(fail)
    }
    fn name(&mut self) -> Result<String, FirewallError> {
        // SAFETY: COM call on a valid element; the caller hashes and drops the string.
        unsafe { self.0.CurrentName() }
            .map(|b| b.to_string())
            .map_err(fail)
    }
    fn rect(&mut self) -> Result<Rect, FirewallError> {
        // SAFETY: COM call on a valid element.
        let r = unsafe { self.0.CurrentBoundingRectangle() }.map_err(fail)?;
        // Same arithmetic as `windows::bounding_rect` (exact equality with the observed rect).
        rect_from_edges(r.left, r.top, r.right, r.bottom).ok_or(FirewallError::Detection)
    }
}

/// Outcome of locating the observed element again.
enum Located {
    Gone,
    Secure,
    Found(IUIAutomationElement),
}

/// Primary approach: `FindFirst(Descendants)` with a runtime-id property condition from the
/// hwnd's element. The hit's runtime id must match exactly. Any failure (including "not found",
/// which windows-rs reports as an error for a null element) returns `None` so the caller falls
/// back to the walk.
fn find_by_property(
    auto: &IUIAutomation,
    root: &IUIAutomationElement,
    rid: &[i32],
) -> Option<IUIAutomationElement> {
    let var = runtime_id_variant(rid)?;
    // SAFETY: COM calls; the variant outlives the call (the condition copies it).
    let cond = unsafe { auto.CreatePropertyCondition(UIA_RuntimeIdPropertyId, &var) }.ok()?;
    // SAFETY: COM call on a valid element and condition.
    let el = unsafe { root.FindFirst(TreeScope_Descendants, &cond) }.ok()?;
    runtime_ids_equal(&runtime_id(&el).ok()?, rid).then_some(el)
}

/// Fallback: bounded, time-boxed subtree walk. `IsPassword` is asked of every node FIRST, a
/// password field is never descended into, and runtime ids are compared. Out of budget is `Err`.
fn find_by_walk(
    root: &IUIAutomationElement,
    all: &IUIAutomationCondition,
    rid: &[i32],
) -> Result<Located, FirewallError> {
    let deadline = Instant::now() + FALLBACK_WALK_BUDGET;
    let kids = |n: &IUIAutomationElement| -> Result<Vec<IUIAutomationElement>, FirewallError> {
        // SAFETY: COM calls on a valid element and condition.
        unsafe {
            let found = n.FindAll(TreeScope_Children, all).map_err(fail)?;
            let len = found.Length().map_err(fail)?;
            (0..len)
                .map(|i| found.GetElement(i).map_err(fail))
                .collect()
        }
    };
    let mut stack: Vec<(IUIAutomationElement, usize)> = Vec::new();
    for k in kids(root)?.into_iter().rev() {
        stack.push((k, 1));
    }
    let mut visited = 0usize;
    while let Some((n, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > MAX_DEPTH || Instant::now() >= deadline {
            return Err(FirewallError::Detection);
        }
        let secure = is_password(&n)?;
        if runtime_id(&n).is_ok_and(|r| runtime_ids_equal(&r, rid)) {
            return Ok(if secure {
                Located::Secure
            } else {
                Located::Found(n)
            });
        }
        if secure {
            continue; // never descend into a password field
        }
        for k in kids(&n)?.into_iter().rev() {
            stack.push((k, depth + 1));
        }
    }
    Ok(Located::Gone)
}

/// Find the observed element again inside `hwnd` (property condition, then the walk).
fn locate(auto: &IUIAutomation, hwnd: u64, rid: &[i32]) -> Result<Located, FirewallError> {
    // SAFETY: COM call; an invalid hwnd fails and is mapped to a fixed error.
    let root = unsafe { auto.ElementFromHandle(hwnd_from_u64(hwnd)) }.map_err(fail)?;
    if let Some(el) = find_by_property(auto, &root, rid) {
        return Ok(Located::Found(el));
    }
    // SAFETY: COM call.
    let all = unsafe { auto.CreateTrueCondition() }.map_err(fail)?;
    find_by_walk(&root, &all, rid)
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WinTargetResolver;

impl TargetResolver for WinTargetResolver {
    fn foreground(&self) -> Result<ForegroundFacts, FirewallError> {
        let (hwnd, pid) = foreground()?;
        let (window, app_id, proc) = window_identity(hwnd, pid)?;
        let integrity = integrity_of(proc.0, true)?;
        Ok(ForegroundFacts {
            window,
            app_id,
            on_user_desktop: input_desktop_is_user_desktop(),
            integrity: integrity.rank(),
            own_integrity: own_integrity()?.rank(),
        })
    }

    fn resolve(&self, req: &ResolveRequest) -> Result<ResolvedTarget, FirewallError> {
        ensure_per_monitor_dpi()?;
        if req.runtime_id.is_empty() {
            return Err(FirewallError::Detection);
        }
        let auto = automation()?;
        match locate(&auto, req.hwnd, &req.runtime_id)? {
            Located::Gone => Ok(ResolvedTarget::Gone),
            Located::Secure => Ok(ResolvedTarget::Secure),
            // `IsPassword` first, then control type, AutomationId hash, Name hash, rect. The
            // element that was read is the one handed to the `LiveTarget`.
            Located::Found(el) => match read_resolved(&mut UiaReads(&el))? {
                ResolvedTarget::Found(mut f) => {
                    f.handle = ElementHandle::from_uia(&el);
                    Ok(ResolvedTarget::Found(f))
                }
                other => Ok(other),
            },
        }
    }

    /// Focus state. NO tree search (`locate`) per call: `GetFocusedElement`, then the pure
    /// `place_focus` (IsPassword first, text-like name check, runtime-id comparison, bounded
    /// parent walk, window root last).
    fn focus_state(&self, hwnd: u64, runtime_id: &[i32]) -> Result<FocusState, FirewallError> {
        let auto = automation()?;
        // SAFETY: COM call. No focused element (or a failed query) is "nothing focused".
        let Ok(focused) = (unsafe { auto.GetFocusedElement() }) else {
            return Ok(FocusState::None);
        };
        // SAFETY: COM call.
        let walker = unsafe { auto.RawViewWalker() }.map_err(fail)?;
        let mut cursor = UiaFocus {
            auto: &auto,
            walker,
            hwnd,
        };
        let placement = place_focus(&mut cursor, &focused, runtime_id)?;
        Ok(placement.into_state(runtime_id.is_empty()))
    }

    fn resolve_window(&self, hwnd: u64, scroll_root: bool) -> Result<ElementHandle, FirewallError> {
        ensure_per_monitor_dpi()?;
        let auto = automation()?;
        // SAFETY: COM call; an invalid hwnd fails and is mapped to a fixed error.
        let root = unsafe { auto.ElementFromHandle(hwnd_from_u64(hwnd)) }.map_err(fail)?;
        if !scroll_root {
            return Ok(ElementHandle::from_uia(&root));
        }
        Ok(scroll_container(&auto, &root)
            .map_or_else(ElementHandle::default, |e| ElementHandle::from_uia(&e)))
    }
}

/// The element that scrolls the window: the window element if it has a ScrollPattern, else the
/// first descendant whose `IsScrollPatternAvailable` is true (a property CONDITION, no content is
/// read). A password field is never returned. `None` when nothing scrolls.
fn scroll_container(
    auto: &IUIAutomation,
    root: &IUIAutomationElement,
) -> Option<IUIAutomationElement> {
    // SAFETY: COM call on a valid element; failure means "no such pattern".
    if unsafe { root.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId) }
        .is_ok()
    {
        return Some(root.clone());
    }
    let var = VARIANT::from(true);
    // SAFETY: COM calls; the variant outlives the call (the condition copies it).
    let cond =
        unsafe { auto.CreatePropertyCondition(UIA_IsScrollPatternAvailablePropertyId, &var) }
            .ok()?;
    // SAFETY: COM call on a valid element and condition; "not found" is an error or null.
    let el = unsafe { root.FindFirst(TreeScope_Descendants, &cond) }.ok()?;
    match is_password(&el) {
        Ok(false) => Some(el),
        _ => None,
    }
}

/// `FocusCursor` over UIA: cheap property reads and the raw-view parent walk.
struct UiaFocus<'a> {
    auto: &'a IUIAutomation,
    walker: IUIAutomationTreeWalker,
    hwnd: u64,
}

impl FocusCursor for UiaFocus<'_> {
    type Node = IUIAutomationElement;

    fn is_password(&mut self, n: &IUIAutomationElement) -> Result<bool, FirewallError> {
        is_password(n)
    }
    fn control_type(&mut self, n: &IUIAutomationElement) -> Result<i32, FirewallError> {
        control_type(n)
    }
    fn name(&mut self, n: &IUIAutomationElement) -> Result<String, FirewallError> {
        // SAFETY: COM call on a valid element; only the lexicon check sees the string.
        unsafe { n.CurrentName() }
            .map(|b| b.to_string())
            .map_err(fail)
    }
    fn runtime_id(&mut self, n: &IUIAutomationElement) -> Result<Vec<i32>, FirewallError> {
        runtime_id(n)
    }
    fn parent(
        &mut self,
        n: &IUIAutomationElement,
    ) -> Result<Option<IUIAutomationElement>, FirewallError> {
        // SAFETY: COM call on a valid element. The end of the chain (and an unreadable parent) is
        // `None`: not inside anything further (fail closed).
        Ok(unsafe { self.walker.GetParentElement(n) }.ok())
    }
    fn window_runtime_id(&mut self) -> Result<Vec<i32>, FirewallError> {
        // SAFETY: COM call; an invalid hwnd fails and is mapped to a fixed error.
        let w = unsafe { self.auto.ElementFromHandle(hwnd_from_u64(self.hwnd)) }.map_err(fail)?;
        runtime_id(&w)
    }
}
