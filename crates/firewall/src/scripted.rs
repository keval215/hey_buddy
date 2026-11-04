//! `ScriptedDesktop`: ONE shared, mutable fake desktop that implements every OS-facing trait the
//! real `PrivacyFirewall` needs (`SecureFieldDetector`, `SessionProbe`, `ScreenTextSource`,
//! `ObservationSource`, `TargetResolver`). Tests mutate the world between `observe` and
//! `revalidate` (rename a label, move a rect, change foreground, lock the session, ...).
//! There is deliberately NO fake `Firewall` (ADR 0001): tests use the real one.
//!
//! The fake behaves like a well-behaved platform adapter: windows have no titles, password
//! elements are reported secure with no label or value, and identity is only given for
//! non-secure elements.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use frame::Rect;

use crate::target::{
    hash_automation_id, hash_label, FocusState, ForegroundFacts, NodeIdentity, ObservationSource,
    ObservedWindow, ResolveRequest, ResolvedFields, ResolvedTarget, TargetResolver, VisibleApp,
    WindowFacts,
};
use crate::{
    FirewallError, NodeRole, RawTextNode, RawWindowTree, ScreenTextSource, SecureFieldDetector,
    SessionProbe, SessionProbeState,
};

/// One element of a fake window (flat list in DFS order; `depth` 1 = child of the window).
#[derive(Clone)]
pub struct ElemSpec {
    pub runtime_id: Vec<i32>,
    pub depth: u16,
    pub role: NodeRole,
    pub label: Option<String>,
    pub value: Option<String>,
    pub editable: bool,
    /// UIA `IsPassword`.
    pub password: bool,
    pub rect: Rect,
    pub control_type: u32,
    pub automation_id: String,
}

impl ElemSpec {
    /// Runtime id `[id]`, depth 1, automation id `auto<id>`, control type 50000 + id.
    pub fn new(id: i32, role: NodeRole, label: &str, rect: Rect) -> Self {
        Self {
            runtime_id: vec![id],
            depth: 1,
            role,
            label: (!label.is_empty()).then(|| label.to_string()),
            value: None,
            editable: matches!(role, NodeRole::TextField),
            password: false,
            rect,
            control_type: 50_000 + id as u32,
            automation_id: format!("auto{id}"),
        }
    }
    pub fn depth(mut self, d: u16) -> Self {
        self.depth = d;
        self
    }
    pub fn value(mut self, v: &str) -> Self {
        self.value = Some(v.to_string());
        self
    }
    pub fn editable(mut self, e: bool) -> Self {
        self.editable = e;
        self
    }
    pub fn password(mut self) -> Self {
        self.password = true;
        self.role = NodeRole::TextField;
        self.editable = true;
        self
    }
}

/// A fake top-level window.
#[derive(Clone)]
pub struct WindowSpec {
    pub hwnd: u64,
    pub app_id: String,
    pub pid: u32,
    pub creation_time: u64,
    pub exe_path: String,
    pub integrity: u32,
    pub rect: Rect,
    pub visible: bool,
    pub elements: Vec<ElemSpec>,
}

impl WindowSpec {
    pub fn new(hwnd: u64, app_id: &str, pid: u32, rect: Rect) -> Self {
        Self {
            hwnd,
            app_id: app_id.to_string(),
            pid,
            creation_time: 1_000 + u64::from(pid),
            exe_path: format!("C:\\Apps\\{app_id}"),
            integrity: 2,
            rect,
            visible: true,
            elements: Vec::new(),
        }
    }
    pub fn element(mut self, e: ElemSpec) -> Self {
        self.elements.push(e);
        self
    }
    pub fn integrity(mut self, i: u32) -> Self {
        self.integrity = i;
        self
    }
}

/// Counters of what the firewall asked the fake desktop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopCalls {
    pub probe: usize,
    pub secure_fields: usize,
    pub tree_reads: usize,
    pub observe_reads: usize,
    pub foreground: usize,
    pub resolve: usize,
    pub focus_state: usize,
}

impl DesktopCalls {
    /// Calls made to the target resolver (foreground + resolve + focus_state).
    pub fn resolver(&self) -> usize {
        self.foreground + self.resolve + self.focus_state
    }
}

struct World {
    windows: Vec<WindowSpec>,
    foreground: u64,
    session: SessionProbeState,
    user_desktop: bool,
    own_integrity: u32,
    focus: Option<(u64, Vec<i32>)>,
    calls: DesktopCalls,
    source_delay: Duration,
    source_panics: bool,
    resolver_delay: Duration,
    resolver_panics: bool,
    /// One-shot: after the next `resolve`, this window becomes the foreground.
    foreground_after_resolve: Option<u64>,
}

/// Cheap to clone; all clones share one world.
#[derive(Clone)]
pub struct ScriptedDesktop {
    world: Arc<Mutex<World>>,
}

impl Default for ScriptedDesktop {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedDesktop {
    pub fn new() -> Self {
        Self {
            world: Arc::new(Mutex::new(World {
                windows: Vec::new(),
                foreground: 0,
                session: SessionProbeState::Normal,
                user_desktop: true,
                own_integrity: 2,
                focus: None,
                calls: DesktopCalls::default(),
                source_delay: Duration::ZERO,
                source_panics: false,
                resolver_delay: Duration::ZERO,
                resolver_panics: false,
                foreground_after_resolve: None,
            })),
        }
    }

    fn w(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add a window; the first one added becomes the foreground window.
    pub fn with_window(self, win: WindowSpec) -> Self {
        {
            let mut w = self.w();
            if w.windows.is_empty() {
                w.foreground = win.hwnd;
            }
            w.windows.push(win);
        }
        self
    }

    pub fn calls(&self) -> DesktopCalls {
        self.w().calls
    }

    // ---- script hooks (call between observe and revalidate) ----

    pub fn set_foreground(&self, hwnd: u64) {
        self.w().foreground = hwnd;
    }
    /// Test hook: right after the next `resolve` returns, `hwnd` becomes the foreground window
    /// (the user switched windows while the element was being re-resolved).
    pub fn set_foreground_after_resolve(&self, hwnd: u64) {
        self.w().foreground_after_resolve = Some(hwnd);
    }
    pub fn set_session(&self, s: SessionProbeState) {
        self.w().session = s;
    }
    pub fn set_user_desktop(&self, on: bool) {
        self.w().user_desktop = on;
    }
    pub fn set_own_integrity(&self, i: u32) {
        self.w().own_integrity = i;
    }
    /// Raise (or lower) a window's process integrity.
    pub fn set_integrity(&self, hwnd: u64, i: u32) {
        self.with_win(hwnd, |win| win.integrity = i);
    }
    /// Another process (new pid / creation time) now owns this hwnd or pid.
    pub fn set_process(&self, hwnd: u64, pid: u32, creation_time: u64) {
        self.with_win(hwnd, |win| {
            win.pid = pid;
            win.creation_time = creation_time;
        });
    }
    pub fn set_exe_path(&self, hwnd: u64, path: &str) {
        self.with_win(hwnd, |win| win.exe_path = path.to_string());
    }
    pub fn rename(&self, hwnd: u64, rid: &[i32], label: &str) {
        self.with_elem(hwnd, rid, |e| e.label = Some(label.to_string()));
    }
    pub fn move_rect(&self, hwnd: u64, rid: &[i32], rect: Rect) {
        self.with_elem(hwnd, rid, |e| e.rect = rect);
    }
    /// The field is now a password field (UIA `IsPassword` true).
    pub fn set_password(&self, hwnd: u64, rid: &[i32], on: bool) {
        self.with_elem(hwnd, rid, |e| e.password = on);
    }
    pub fn set_control_type(&self, hwnd: u64, rid: &[i32], ct: u32) {
        self.with_elem(hwnd, rid, |e| e.control_type = ct);
    }
    pub fn set_automation_id(&self, hwnd: u64, rid: &[i32], id: &str) {
        self.with_elem(hwnd, rid, |e| e.automation_id = id.to_string());
    }
    pub fn remove_element(&self, hwnd: u64, rid: &[i32]) {
        self.with_win(hwnd, |win| win.elements.retain(|e| e.runtime_id != rid));
    }
    /// Swap the element for a different one (new runtime id, role, label, ...).
    pub fn replace_element(&self, hwnd: u64, rid: &[i32], new: ElemSpec) {
        self.with_win(hwnd, |win| {
            if let Some(e) = win.elements.iter_mut().find(|e| e.runtime_id == rid) {
                *e = new;
            }
        });
    }
    /// Keyboard focus is on element `rid` of window `hwnd` (`None` = nothing focused).
    pub fn set_focus(&self, hwnd: u64, rid: Option<&[i32]>) {
        self.w().focus = rid.map(|r| (hwnd, r.to_vec()));
    }
    /// Set (or clear) the value text of an element (fake actuators; a non-blank value shows up
    /// as `has_value`).
    pub fn set_value(&self, hwnd: u64, rid: &[i32], v: Option<&str>) {
        self.with_elem(hwnd, rid, |e| e.value = v.map(str::to_string));
    }
    /// The value text of an element (tests only).
    pub fn value_of(&self, hwnd: u64, rid: &[i32]) -> Option<String> {
        let w = self.w();
        w.windows
            .iter()
            .find(|x| x.hwnd == hwnd)
            .and_then(|x| x.elements.iter().find(|e| e.runtime_id == rid))
            .and_then(|e| e.value.clone())
    }
    /// Add a window without changing the foreground (a launched app).
    pub fn add_window(&self, win: WindowSpec) {
        self.w().windows.push(win);
    }
    pub fn foreground_hwnd(&self) -> u64 {
        self.w().foreground
    }
    /// Where keyboard focus is now (tests only).
    pub fn focused(&self) -> Option<(u64, Vec<i32>)> {
        self.w().focus.clone()
    }
    /// Hwnd of the topmost visible window of `app_id`.
    pub fn hwnd_of_app(&self, app_id: &str) -> Option<u64> {
        let w = self.w();
        w.windows
            .iter()
            .rev()
            .find(|x| x.visible && x.app_id.eq_ignore_ascii_case(app_id))
            .map(|x| x.hwnd)
    }
    /// Number of windows in the world (tests only).
    pub fn window_count(&self) -> usize {
        self.w().windows.len()
    }
    /// Runtime id of the element of `hwnd` whose rect is exactly `rect` (fake actuators resolve a
    /// live target to a world element this way; the real target table is private).
    pub fn element_at(&self, hwnd: u64, rect: Rect) -> Option<Vec<i32>> {
        let w = self.w();
        w.windows
            .iter()
            .find(|x| x.hwnd == hwnd)
            .and_then(|x| x.elements.iter().find(|e| e.rect == rect))
            .map(|e| e.runtime_id.clone())
    }
    pub fn slow_source(&self, d: Duration) {
        self.w().source_delay = d;
    }
    pub fn panicking_source(&self, on: bool) {
        self.w().source_panics = on;
    }
    pub fn slow_resolver(&self, d: Duration) {
        self.w().resolver_delay = d;
    }
    pub fn panicking_resolver(&self, on: bool) {
        self.w().resolver_panics = on;
    }

    fn with_win(&self, hwnd: u64, f: impl FnOnce(&mut WindowSpec)) {
        if let Some(win) = self.w().windows.iter_mut().find(|x| x.hwnd == hwnd) {
            f(win);
        }
    }
    fn with_elem(&self, hwnd: u64, rid: &[i32], f: impl FnOnce(&mut ElemSpec)) {
        self.with_win(hwnd, |win| {
            if let Some(e) = win.elements.iter_mut().find(|e| e.runtime_id == rid) {
                f(e);
            }
        });
    }

    // ---- reads ----

    /// The tree and identities of the foreground window, as a well-behaved adapter reads them.
    fn read_foreground(w: &World) -> Result<ObservedWindow, FirewallError> {
        let win = w
            .windows
            .iter()
            .find(|x| x.hwnd == w.foreground && x.visible)
            .ok_or(FirewallError::Detection)?;
        let mut nodes = vec![RawTextNode {
            depth: 0,
            role: NodeRole::Window,
            secure: false,
            editable: false,
            label: None,
            value: None,
            has_value: false,
            rect: Some(win.rect),
        }];
        let mut identities: Vec<Option<NodeIdentity>> = vec![None];
        for e in &win.elements {
            if e.password {
                nodes.push(RawTextNode {
                    depth: e.depth,
                    role: NodeRole::TextField,
                    secure: true,
                    editable: true,
                    label: None,
                    value: None,
                    has_value: false,
                    rect: Some(e.rect),
                });
                identities.push(None);
                continue;
            }
            let is_static = matches!(e.role, NodeRole::StaticText | NodeRole::Heading);
            nodes.push(RawTextNode {
                depth: e.depth,
                role: e.role,
                secure: false,
                editable: e.editable,
                label: e.label.clone(),
                value: if is_static { e.value.clone() } else { None },
                has_value: e.value.as_deref().is_some_and(|v| !v.trim().is_empty()),
                rect: Some(e.rect),
            });
            identities.push(Some(NodeIdentity {
                runtime_id: e.runtime_id.clone(),
                control_type: e.control_type,
                automation_id_hash: hash_automation_id(&e.automation_id),
                label_hash: hash_label(e.label.as_deref()),
            }));
        }
        let tree = RawWindowTree {
            app_id: win.app_id.clone(),
            window_rect: win.rect,
            nodes,
        };
        let visible_apps = w
            .windows
            .iter()
            .filter(|x| x.visible)
            .map(|x| VisibleApp {
                app_id: x.app_id.clone(),
                pid: x.pid,
            })
            .collect();
        Ok(ObservedWindow {
            tree,
            identities,
            window: window_facts(win),
            visible_apps,
        })
    }
}

impl ScriptedDesktop {
    fn resolve_in(w: &World, req: &ResolveRequest) -> ResolvedTarget {
        let Some(win) = w.windows.iter().find(|x| x.hwnd == req.hwnd) else {
            return ResolvedTarget::Gone;
        };
        let Some(e) = win.elements.iter().find(|e| e.runtime_id == req.runtime_id) else {
            return ResolvedTarget::Gone;
        };
        // is_password FIRST; nothing else is read for a password field.
        if e.password {
            return ResolvedTarget::Secure;
        }
        ResolvedTarget::Found(ResolvedFields {
            control_type: e.control_type,
            automation_id_hash: hash_automation_id(&e.automation_id),
            label_hash: hash_label(e.label.as_deref()),
            rect: e.rect,
        })
    }
}

fn window_facts(win: &WindowSpec) -> WindowFacts {
    WindowFacts {
        hwnd: win.hwnd,
        pid: win.pid,
        process_creation_time: win.creation_time,
        exe_path: win.exe_path.clone(),
    }
}

impl SecureFieldDetector for ScriptedDesktop {
    fn secure_fields(&self) -> Result<Vec<Rect>, FirewallError> {
        let mut w = self.w();
        w.calls.secure_fields += 1;
        Ok(w.windows
            .iter()
            .filter(|x| x.visible)
            .flat_map(|x| x.elements.iter().filter(|e| e.password).map(|e| e.rect))
            .collect())
    }
}

impl SessionProbe for ScriptedDesktop {
    fn state(&self) -> Result<SessionProbeState, FirewallError> {
        let mut w = self.w();
        w.calls.probe += 1;
        Ok(w.session)
    }
}

impl ScreenTextSource for ScriptedDesktop {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError> {
        let mut w = self.w();
        w.calls.tree_reads += 1;
        Self::read_foreground(&w).map(|o| o.tree)
    }
}

impl ObservationSource for ScriptedDesktop {
    fn observe_foreground(&self) -> Result<ObservedWindow, FirewallError> {
        let (delay, panics) = {
            let mut w = self.w();
            w.calls.observe_reads += 1;
            (w.source_delay, w.source_panics)
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if panics {
            panic!("scripted observation source panic");
        }
        Self::read_foreground(&self.w())
    }
}

impl TargetResolver for ScriptedDesktop {
    fn foreground(&self) -> Result<ForegroundFacts, FirewallError> {
        let (delay, panics) = {
            let mut w = self.w();
            w.calls.foreground += 1;
            (w.resolver_delay, w.resolver_panics)
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if panics {
            panic!("scripted resolver panic");
        }
        let w = self.w();
        let win = w
            .windows
            .iter()
            .find(|x| x.hwnd == w.foreground)
            .ok_or(FirewallError::Detection)?;
        Ok(ForegroundFacts {
            window: window_facts(win),
            app_id: win.app_id.to_lowercase(),
            on_user_desktop: w.user_desktop,
            integrity: win.integrity,
            own_integrity: w.own_integrity,
        })
    }

    fn resolve(&self, req: &ResolveRequest) -> Result<ResolvedTarget, FirewallError> {
        let (delay, panics) = {
            let mut w = self.w();
            w.calls.resolve += 1;
            (w.resolver_delay, w.resolver_panics)
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if panics {
            panic!("scripted resolver panic");
        }
        let mut w = self.w();
        let result = Self::resolve_in(&w, req);
        if let Some(h) = w.foreground_after_resolve.take() {
            w.foreground = h;
        }
        Ok(result)
    }

    fn focus_state(&self, hwnd: u64, runtime_id: &[i32]) -> Result<FocusState, FirewallError> {
        let (delay, panics) = {
            let mut w = self.w();
            w.calls.focus_state += 1;
            (w.resolver_delay, w.resolver_panics)
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if panics {
            panic!("scripted resolver panic");
        }
        let w = self.w();
        let Some((fh, frid)) = w.focus.clone() else {
            return Ok(FocusState::None);
        };
        if fh != hwnd {
            return Ok(FocusState::Other);
        }
        let focused = w
            .windows
            .iter()
            .find(|x| x.hwnd == fh)
            .and_then(|x| x.elements.iter().find(|e| e.runtime_id == frid));
        match focused {
            None => Ok(FocusState::None),
            Some(e) if e.password => Ok(FocusState::Secure),
            // Same rule as the Windows resolver: text-like focused elements whose label names a
            // secret / one-time code (the label is lexicon-checked here, never returned).
            Some(e)
                if (e.editable || matches!(e.role, NodeRole::TextField | NodeRole::ComboBox))
                    && e.label
                        .as_deref()
                        .is_some_and(crate::sensitive::sensitive_label) =>
            {
                Ok(FocusState::Sensitive)
            }
            Some(_) if runtime_id.is_empty() => Ok(FocusState::Descendant),
            Some(_) if frid == runtime_id => Ok(FocusState::Target),
            // Runtime ids are flat in the fake: "descendant" = longer id with the target as prefix.
            Some(_) if frid.starts_with(runtime_id) => Ok(FocusState::Descendant),
            Some(_) => Ok(FocusState::Other),
        }
    }
}
