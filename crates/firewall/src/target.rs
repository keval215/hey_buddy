//! Targets: what the firewall learns about a node's IDENTITY (never its content), the
//! firewall-internal [`ObservationSource`] / [`TargetResolver`] traits, [`TargetRefusal`] and the
//! sealed [`LiveTarget`] (ADR 0004 §5-§6).
//!
//! Identity data is read ONLY after `is_secure` returned false (`TreeCursor::identity`) and, at
//! revalidation, `is_password` is read FIRST. Hashes are SHA-256; raw automation ids and names
//! never leave the platform adapter.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use frame::Rect;
use sha2::{Digest, Sha256};

use crate::error::{BlockReason, FirewallError, PauseReason};
use crate::observe::{ElementRef, Observation};
use crate::text::RawWindowTree;

/// SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Hash of an element name as compared by `revalidate`. `None` and whitespace-only names hash
/// like the empty string, so "no name" is stable.
pub fn hash_label(label: Option<&str>) -> [u8; 32] {
    let l = label.map(str::trim).unwrap_or("");
    sha256(l.as_bytes())
}

/// Hash of an automation id (platform adapters use this so both sides hash alike).
pub fn hash_automation_id(id: &str) -> [u8; 32] {
    sha256(id.as_bytes())
}

/// Identity facts of one emitted, NON-secure node.
#[derive(Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    /// UIA runtime id (opaque, only meaningful inside the observed hwnd).
    pub runtime_id: Vec<i32>,
    pub control_type: u32,
    pub automation_id_hash: [u8; 32],
    /// Filled in by the tree walker from the label it already read (hash of the empty string
    /// for nodes whose label is never read). Cursors leave it zeroed.
    pub label_hash: [u8; 32],
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeIdentity(control {})", self.control_type)
    }
}

/// Facts about a top-level window and its process. Never a title.
#[derive(Clone, PartialEq, Eq)]
pub struct WindowFacts {
    pub hwnd: u64,
    pub pid: u32,
    /// Process creation time (FILETIME ticks); corroborates `pid` against recycling.
    pub process_creation_time: u64,
    /// Local image path of the process (shown to the user in the approval UI, never to a model).
    pub exe_path: String,
}

impl fmt::Debug for WindowFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WindowFacts(pid {})", self.pid)
    }
}

/// A visible top-level app: app id and pid only.
#[derive(Clone, PartialEq, Eq)]
pub struct VisibleApp {
    pub app_id: String,
    pub pid: u32,
}

impl fmt::Debug for VisibleApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VisibleApp(pid {})", self.pid)
    }
}

/// What an [`ObservationSource`] returns: the `check_text` tree plus identity data.
#[derive(Clone)]
pub struct ObservedWindow {
    pub tree: RawWindowTree,
    /// Parallel to `tree.nodes`. `None` for secure nodes and for nodes whose identity was not
    /// read. The firewall ignores an identity attached to a secure node.
    pub identities: Vec<Option<NodeIdentity>>,
    pub window: WindowFacts,
    pub visible_apps: Vec<VisibleApp>,
}

impl fmt::Debug for ObservedWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObservedWindow({} nodes)", self.tree.nodes.len())
    }
}

/// Reads the foreground window for `observe`: the `ScreenTextSource` contract (foreground
/// window only, secure-first, no titles, no secure values) PLUS per-node identity read only
/// after `is_secure` returned false. The firewall re-enforces everything it can.
pub trait ObservationSource: Send + Sync {
    fn observe_foreground(&self) -> Result<ObservedWindow, FirewallError>;
}

/// Source for platforms without an implementation: `observe` blocks with `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedObservationSource;

impl ObservationSource for UnsupportedObservationSource {
    fn observe_foreground(&self) -> Result<ObservedWindow, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

/// Foreground window facts for `revalidate` / `verify_focus`. No titles.
#[derive(Clone, PartialEq, Eq)]
pub struct ForegroundFacts {
    pub window: WindowFacts,
    /// Lowercase exe name / bundle id.
    pub app_id: String,
    /// The input desktop is the user's normal desktop (not Winlogon / UAC).
    pub on_user_desktop: bool,
    /// Integrity level (higher = more privileged) of the target process and of ours.
    pub integrity: u32,
    pub own_integrity: u32,
}

impl fmt::Debug for ForegroundFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ForegroundFacts(pid {})", self.window.pid)
    }
}

/// One element to re-resolve, scoped to the observed window. The resolver hashes the element's
/// name (never returned). Only emitted nodes are ever resolved, and those are never title-ish.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolveRequest {
    pub hwnd: u64,
    pub runtime_id: Vec<i32>,
}

/// The platform element a resolver re-resolved, handed to the [`LiveTarget`] so the driver acts
/// on exactly the element the firewall checked (no second search). Empty on every platform but
/// Windows, where it is an agile COM reference (safe to move between the resolver's deadline
/// worker and the driver's worker). Opaque: `Debug` prints a fixed token, equality ignores it.
#[derive(Clone, Default)]
pub struct ElementHandle {
    #[cfg(target_os = "windows")]
    agile: Option<
        windows::core::AgileReference<windows::Win32::UI::Accessibility::IUIAutomationElement>,
    >,
}

impl ElementHandle {
    /// An agile reference to `el`; empty when COM refuses (the target then has no element and the
    /// actuator fails closed).
    #[cfg(target_os = "windows")]
    pub(crate) fn from_uia(el: &windows::Win32::UI::Accessibility::IUIAutomationElement) -> Self {
        Self {
            agile: windows::core::AgileReference::new(el).ok(),
        }
    }
}

impl PartialEq for ElementHandle {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for ElementHandle {}

impl fmt::Debug for ElementHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ElementHandle")
    }
}

/// Identity of a re-resolved element.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedFields {
    pub control_type: u32,
    pub automation_id_hash: [u8; 32],
    pub label_hash: [u8; 32],
    pub rect: Rect,
    /// The element itself (Windows), for [`LiveTarget`]. Not part of the identity comparison.
    pub handle: ElementHandle,
}

/// Result of re-resolving an element. `Secure` carries nothing else: when `is_password` is
/// true the resolver must not read anything further.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    Gone,
    Secure,
    Found(ResolvedFields),
}

/// Where keyboard focus is, relative to the target (decided by the resolver; `is_password` of
/// the focused element is read first and yields `Secure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusState {
    /// Nothing focused.
    None,
    /// The focused element is a password field.
    Secure,
    /// The focused element is a text field whose label names a secret or one-time code (OTP,
    /// PIN, ...) although it is not `IsPassword`. Decided inside the resolver on the label it
    /// just read for the focused element. Takes precedence over `Target` / `Descendant`.
    Sensitive,
    /// The focused element is the target.
    Target,
    /// The focused element is inside the target (for the window target: anywhere inside it).
    Descendant,
    /// Focus is somewhere else.
    Other,
}

/// Re-resolves one element for `revalidate` and answers focus questions. Every method is called
/// on a deadline worker. Implementations read identity only.
pub trait TargetResolver: Send + Sync {
    fn foreground(&self) -> Result<ForegroundFacts, FirewallError>;
    /// Re-resolve inside `req.hwnd` only. Reads `is_password` FIRST.
    fn resolve(&self, req: &ResolveRequest) -> Result<ResolvedTarget, FirewallError>;
    /// Focus state relative to the element `runtime_id` inside `hwnd` (empty id = the window).
    fn focus_state(&self, hwnd: u64, runtime_id: &[i32]) -> Result<FocusState, FirewallError>;
    /// The element of the observed window itself, for the window targets (Windows only; the
    /// firewall calls this only there). With `scroll_root` the element is the window's scroll
    /// container (the window element if it scrolls, else its first scrollable descendant);
    /// an empty handle when there is none. Default: empty.
    fn resolve_window(
        &self,
        _hwnd: u64,
        _scroll_root: bool,
    ) -> Result<ElementHandle, FirewallError> {
        Ok(ElementHandle::default())
    }
}

/// Resolver for platforms without an implementation: every call is `Err(Unsupported)`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedTargetResolver;

impl TargetResolver for UnsupportedTargetResolver {
    fn foreground(&self) -> Result<ForegroundFacts, FirewallError> {
        Err(FirewallError::Unsupported)
    }
    fn resolve(&self, _req: &ResolveRequest) -> Result<ResolvedTarget, FirewallError> {
        Err(FirewallError::Unsupported)
    }
    fn focus_state(&self, _hwnd: u64, _id: &[i32]) -> Result<FocusState, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

/// What to revalidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSel {
    Element(ElementRef),
    /// The observed window as scroll root (allowed on payment pages).
    ScrollRoot,
    /// The observed window itself.
    Window,
}

/// Why a target was refused. Fixed tokens only: never labels, titles or OS text (L-010).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetRefusal {
    #[error("stale reference")]
    StaleRef,
    /// `field` is a fixed name: `app`, `control_type`, `automation_id`, `label`, `rect`.
    #[error("target changed: {field}")]
    TargetChanged { field: &'static str },
    #[error("app is deny-listed")]
    DeniedApp,
    #[error("app is restricted")]
    RestrictedApp,
    #[error("authentication surface")]
    AuthSurface,
    #[error("own window")]
    OwnWindow,
    #[error("target is elevated")]
    ElevatedTarget,
    #[error("secure target")]
    SecureTarget,
    #[error("sensitive target")]
    SensitiveTarget,
    #[error("payment page suspected")]
    PaymentSuspected,
    #[error("target is not editable")]
    NotEditable,
    #[error("session paused")]
    SessionPaused(PauseReason),
    #[error("blocked by the firewall")]
    Blocked(BlockReason),
    #[error("foreground window changed")]
    ForegroundChanged,
    #[error("focus lost")]
    FocusLost,
    #[error("no semantic path")]
    NoSemanticPath,
    #[error("unsupported")]
    Unsupported,
}

/// Checks the facts `verify_focus` needs; implemented inside the firewall (probe + resolver on
/// deadline workers). Private so a `LiveTarget` cannot be built around a foreign guard.
pub(crate) trait FocusGuard: Send + Sync {
    fn verify(
        &self,
        hwnd_identity: &WindowFacts,
        runtime_id: &[i32],
        within: bool,
    ) -> Result<(), TargetRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    Element,
    ScrollRoot,
    Window,
}

/// A target that passed `revalidate` a moment ago. Sealed: private fields and a `pub(crate)`
/// constructor, so only [`crate::Firewall::revalidate`] can mint one. `!Send + !Sync`: it is used
/// on the worker thread that revalidated it, immediately, and cannot be stored across threads.
/// It borrows the [`Observation`] it came from.
///
/// The `driver` (S2-14c) must call [`LiveTarget::verify_focus`] before every input batch.
///
/// ```compile_fail,E0451
/// // Struct-literal construction from outside the crate: private fields.
/// fn forge<'o>() -> firewall::LiveTarget<'o> {
///     firewall::LiveTarget {
///         observation: todo!(),
///         element: todo!(),
///         kind: todo!(),
///         window: todo!(),
///         runtime_id: todo!(),
///         guard: todo!(),
///         _not_send: todo!(),
///     }
/// }
/// ```
///
/// ```compile_fail,E0624
/// // The constructor is `pub(crate)`.
/// fn forge<'o>(o: &'o firewall::Observation) -> firewall::LiveTarget<'o> {
///     firewall::LiveTarget::new(o, todo!(), todo!(), todo!(), todo!(), todo!(), todo!())
/// }
/// ```
///
/// ```compile_fail,E0277
/// // `!Send`: cannot be moved to another thread.
/// fn leak(t: firewall::LiveTarget<'static>) {
///     std::thread::spawn(move || {
///         let _t = t;
///     });
/// }
/// ```
pub struct LiveTarget<'o> {
    observation: &'o Observation,
    element: Option<ElementRef>,
    kind: TargetKind,
    window: WindowFacts,
    runtime_id: Vec<i32>,
    guard: Arc<dyn FocusGuard>,
    /// The element the resolver re-resolved (Windows). Used only by the driver's Windows actuator.
    #[cfg(target_os = "windows")]
    uia: Option<windows::Win32::UI::Accessibility::IUIAutomationElement>,
    _not_send: PhantomData<*const ()>,
}

impl<'o> LiveTarget<'o> {
    pub(crate) fn new(
        observation: &'o Observation,
        element: Option<ElementRef>,
        kind: TargetKind,
        window: WindowFacts,
        runtime_id: Vec<i32>,
        guard: Arc<dyn FocusGuard>,
        handle: ElementHandle,
    ) -> Self {
        #[cfg(not(target_os = "windows"))]
        let _ = handle;
        Self {
            observation,
            element,
            kind,
            window,
            runtime_id,
            guard,
            // Resolved HERE, on the thread that owns this target (the driver's worker).
            #[cfg(target_os = "windows")]
            uia: handle.agile.as_ref().and_then(|a| a.resolve().ok()),
            _not_send: PhantomData,
        }
    }

    /// The UIA element the firewall re-resolved and checked for this target (the window / scroll
    /// container element for the window targets). `None` when there is none: the actuator then
    /// fails closed. ADR 0004 section 6: used ONLY by `crates/driver/src/platform/windows.rs`
    /// (a CI grep enforces it); never to read anything.
    #[cfg(target_os = "windows")]
    pub fn uia_element(&self) -> Option<&windows::Win32::UI::Accessibility::IUIAutomationElement> {
        self.uia.as_ref()
    }

    pub fn observation(&self) -> &'o Observation {
        self.observation
    }

    /// The element this handle stands for; `None` for the window / scroll-root targets.
    pub fn element(&self) -> Option<ElementRef> {
        self.element
    }

    /// Foreground unchanged, session Normal, and the focused element IS the target (the window
    /// targets: focus is somewhere inside the window). A focused password field refuses.
    pub fn verify_focus(&self) -> Result<(), TargetRefusal> {
        self.guard.verify(
            &self.window,
            &self.runtime_id,
            self.kind != TargetKind::Element,
        )
    }

    /// Like [`verify_focus`](Self::verify_focus) but the focused element may be a descendant of
    /// the target (key combos).
    pub fn verify_focus_within(&self) -> Result<(), TargetRefusal> {
        self.guard.verify(&self.window, &self.runtime_id, true)
    }
}

impl fmt::Debug for LiveTarget<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LiveTarget({:?})", self.kind)
    }
}
