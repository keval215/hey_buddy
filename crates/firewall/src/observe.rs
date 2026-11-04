//! Agent observation (ADR 0004 §5): the sealed [`Observation`] (screen-text v1 plus a private
//! target table), [`ElementRef`], [`ObservationId`], [`TargetDescriptor`] and [`AppId`].
//!
//! An `Observation` is immutable. "One observation authorises at most ONE action" is enforced by
//! the driver (S2-14c), which holds the latest observation and spends it; this module offers no
//! `spent` flag on purpose.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use frame::Rect;

use crate::builtin;
use crate::error::{BlockReason, PauseReason};
use crate::redacted_text::RedactedScreenText;
use crate::target::{NodeIdentity, WindowFacts};
use crate::text::{NodeRole, WithheldReason};

/// Process-wide random nonce: refs from another process or run can never match.
fn process_nonce() -> Option<[u8; 16]> {
    static NONCE: OnceLock<Option<[u8; 16]>> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok().map(|()| b)
    })
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Identifies one observation: a per-process random nonce plus a monotonic generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObservationId {
    nonce: [u8; 16],
    generation: u64,
}

impl ObservationId {
    /// `None` when the OS RNG fails (observe then fails closed).
    pub(crate) fn fresh() -> Option<Self> {
        let nonce = process_nonce()?;
        Some(Self {
            nonce,
            generation: GENERATION.fetch_add(1, Ordering::SeqCst),
        })
    }

    /// Fixed ids for tests and for the `action` crate's golden vectors (feature `fakes`).
    #[cfg(any(test, feature = "fakes"))]
    pub fn from_parts(nonce: [u8; 16], generation: u64) -> Self {
        Self { nonce, generation }
    }

    /// `nonce` (16 bytes) followed by the generation, little-endian.
    pub fn canonical(&self) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[..16].copy_from_slice(&self.nonce);
        out[16..].copy_from_slice(&self.generation.to_le_bytes());
        out
    }
}

impl fmt::Debug for ObservationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObservationId(gen {})", self.generation)
    }
}

/// A reference to one numbered node (`#n` in the screen text) of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementRef {
    pub observation: ObservationId,
    /// The `#n` of the screen text (1-based).
    pub node: u32,
}

impl ElementRef {
    /// `ObservationId::canonical` followed by the node, little-endian.
    pub fn canonical(&self) -> [u8; 28] {
        let mut out = [0u8; 28];
        out[..24].copy_from_slice(&self.observation.canonical());
        out[24..].copy_from_slice(&self.node.to_le_bytes());
        out
    }
}

/// Application identifier: lowercase exe name (Windows) or bundle id. Validated: 1-128
/// characters, alphanumeric (any script) or one of `. _ - + @ ( )` and space; no path
/// separators, URL syntax or hostile characters. Never a window title.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId(String);

pub const MAX_APP_ID_CHARS: usize = 128;

impl AppId {
    /// Lowercases and validates; `None` when invalid.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim().to_lowercase();
        let n = s.chars().count();
        if n == 0 || n > MAX_APP_ID_CHARS || s.contains("://") {
            return None;
        }
        let ok = s.chars().all(|c| {
            (c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '(' | ')' | ' '))
                && !crate::text::is_hostile_char(c)
        });
        // No leading/trailing/double spaces: the string must round-trip through sanitise.
        (ok && crate::text::sanitise(&s) == s).then_some(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationFlags {
    /// A card / bank-account label was seen on screen (heuristic, English only).
    pub payment_suspected: bool,
    /// Number of secure (password) fields hidden in the text.
    pub secure_fields: u32,
}

/// Shown in the approval UI and bound into the action digest. All values are firewall-cleared
/// (the label is the same sanitised, scrubbed label that appears in the screen text).
#[derive(Clone)]
pub struct TargetDescriptor {
    pub element: ElementRef,
    pub app_id: AppId,
    pub role: NodeRole,
    pub label: Option<Arc<str>>,
    /// SHA-256 of the element's name as compared by `revalidate`.
    pub label_sha256: [u8; 32],
    pub rect: Rect,
    pub editable: bool,
    pub sensitive_label: bool,
}

impl TargetDescriptor {
    /// ADR 0004 §7 (little-endian; `lp(x)` = u64 length followed by the bytes):
    /// `0x01` ‖ element (28 B) ‖ lp(app_id) ‖ lp(role) ‖ label_sha256 ‖ i32 x ‖ i32 y ‖ u32 w ‖
    /// u32 h ‖ u8 editable ‖ u8 sensitive_label.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        fn lp(out: &mut Vec<u8>, b: &[u8]) {
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
        let mut out = vec![0x01];
        out.extend_from_slice(&self.element.canonical());
        lp(&mut out, self.app_id.as_str().as_bytes());
        lp(&mut out, self.role.name().as_bytes());
        out.extend_from_slice(&self.label_sha256);
        out.extend_from_slice(&self.rect.x.to_le_bytes());
        out.extend_from_slice(&self.rect.y.to_le_bytes());
        out.extend_from_slice(&self.rect.w.to_le_bytes());
        out.extend_from_slice(&self.rect.h.to_le_bytes());
        out.push(u8::from(self.editable));
        out.push(u8::from(self.sensitive_label));
        out
    }
}

impl fmt::Debug for TargetDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TargetDescriptor(#{}, {:?}, editable {})",
            self.element.node, self.role, self.editable
        )
    }
}

/// Window identity bound into an observation. Private.
#[derive(Clone)]
pub(crate) struct WindowIdentity {
    pub(crate) facts: WindowFacts,
}

/// One emitted line's targeting data. Private.
#[derive(Clone)]
pub(crate) struct TargetSlot {
    pub(crate) identity: NodeIdentity,
    pub(crate) role: NodeRole,
    pub(crate) editable: bool,
    pub(crate) label: Option<Arc<str>>,
    pub(crate) rect: Rect,
    pub(crate) sensitive_label: bool,
}

/// What `#n` can be addressed as.
#[derive(Clone)]
pub(crate) enum Slot {
    /// Secure node: can never be targeted.
    Secure,
    /// Emitted but no identity was read: not addressable.
    Unaddressable,
    Target(TargetSlot),
}

/// What the agent engine sees of the screen: screen-text v1 (plus an `apps:` line) and a private
/// target table. Sealed: private fields, `pub(crate)` constructor, no `Clone`, `Default` or
/// serde; `Debug` prints counts only.
///
/// Immutable. The driver (S2-14c) holds the latest one and spends it on its first action.
///
/// ```compile_fail,E0451
/// // Struct-literal construction from outside the crate: private fields.
/// fn forge() -> firewall::Observation {
///     firewall::Observation {
///         id: todo!(),
///         text: todo!(),
///         targets: todo!(),
///         window: todo!(),
///         app_id: todo!(),
///         apps: todo!(),
///         flags: todo!(),
///         taken: todo!(),
///     }
/// }
/// ```
///
/// ```compile_fail,E0599
/// // No Default.
/// let _ = firewall::Observation::default();
/// ```
///
/// ```compile_fail,E0599
/// // No Clone.
/// fn dup(o: firewall::Observation) -> firewall::Observation {
///     o.clone()
/// }
/// ```
pub struct Observation {
    id: ObservationId,
    text: RedactedScreenText,
    /// Index = `#n - 1`.
    targets: Box<[Slot]>,
    window: WindowIdentity,
    app_id: AppId,
    apps: Arc<[AppId]>,
    flags: ObservationFlags,
    taken: Instant,
}

impl Observation {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: ObservationId,
        text: RedactedScreenText,
        targets: Box<[Slot]>,
        window: WindowIdentity,
        app_id: AppId,
        apps: Arc<[AppId]>,
        flags: ObservationFlags,
    ) -> Self {
        Self {
            id,
            text,
            targets,
            window,
            app_id,
            apps,
            flags,
            taken: Instant::now(),
        }
    }

    pub fn id(&self) -> ObservationId {
        self.id
    }

    /// Screen-text v1 plus an `apps:` line after the `app:` header.
    pub fn text(&self) -> &RedactedScreenText {
        &self.text
    }

    /// Visible top-level apps that are not deny-listed, not authentication surfaces and not ours.
    pub fn apps(&self) -> &[AppId] {
        &self.apps
    }

    pub fn flags(&self) -> ObservationFlags {
        self.flags
    }

    /// The foreground app this observation describes.
    pub fn app_id(&self) -> &AppId {
        &self.app_id
    }

    /// Time since the observation was taken (the driver enforces its maximum age).
    pub fn age(&self) -> Duration {
        self.taken.elapsed()
    }

    /// Reference to node `#node`; `None` for secure nodes, unaddressable nodes and numbers out
    /// of range.
    pub fn element(&self, node: u32) -> Option<ElementRef> {
        match self.slot(node)? {
            Slot::Target(_) => Some(ElementRef {
                observation: self.id,
                node,
            }),
            _ => None,
        }
    }

    /// Descriptor of a referenced node. `None` for another observation's ref, unknown or secure
    /// nodes, and for nodes of a restricted app (observable, no actions).
    pub fn describe(&self, r: ElementRef) -> Option<TargetDescriptor> {
        if r.observation != self.id || builtin::is_restricted(self.app_id.as_str()) {
            return None;
        }
        let Slot::Target(t) = self.slot(r.node)? else {
            return None;
        };
        Some(TargetDescriptor {
            element: r,
            app_id: self.app_id.clone(),
            role: t.role,
            label: t.label.clone(),
            label_sha256: t.identity.label_hash,
            rect: t.rect,
            editable: t.editable,
            sensitive_label: t.sensitive_label,
        })
    }

    /// Descriptors of every addressable node, in emission order (`#1`, `#2`, ...). Secure and
    /// unaddressable nodes are skipped; a restricted app (observable, no actions) yields none.
    /// Read-only enumeration for `policy::bind_step`; it builds nothing sealed.
    pub fn elements(&self) -> impl Iterator<Item = TargetDescriptor> + '_ {
        (1..=self.targets.len() as u32).filter_map(|node| {
            let r = self.element(node)?;
            self.describe(r)
        })
    }

    /// The foreground app is on the built-in restricted list (ADR 0004 section 8): observable,
    /// never actionable.
    pub fn is_restricted_app(&self) -> bool {
        builtin::is_restricted(self.app_id.as_str())
    }

    pub(crate) fn slot(&self, node: u32) -> Option<&Slot> {
        let i = usize::try_from(node).ok()?.checked_sub(1)?;
        self.targets.get(i)
    }

    pub(crate) fn window(&self) -> &WindowIdentity {
        &self.window
    }
}

impl fmt::Debug for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let addressable = self
            .targets
            .iter()
            .filter(|s| matches!(s, Slot::Target(_)))
            .count();
        write!(
            f,
            "Observation(gen {}, {} nodes, {} targets, {} apps)",
            self.id.generation,
            self.targets.len(),
            addressable,
            self.apps.len()
        )
    }
}

/// Result of `Firewall::observe`.
#[derive(Debug)]
pub enum ObserveVerdict {
    Clear(Observation),
    Withheld(WithheldReason),
    Paused(PauseReason),
    Blocked(BlockReason),
}
