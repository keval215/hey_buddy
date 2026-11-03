//! Text-first screen context (ADR 0002 Addendum A): raw accessibility-tree types supplied by a
//! [`ScreenTextSource`], and the pure [`redact_tree`] that turns one into [`TextVerdict`].
//!
//! The source is NOT trusted to follow its contract: `redact_tree` enforces every privacy rule
//! itself (secure nodes are never read, editable values never emitted, titles dropped, text
//! sanitised) so that a misbehaving source cannot leak.
//!
//! # Format "screen-text v1"
//! ```text
//! app: com.example.app
//! #1 text field "Email" [has text]
//! #2 password field [hidden]
//! #3 button "Sign in"
//! ```
//! DFS order, fixed English role names, app id included, window/page titles never.

use std::fmt;
use std::sync::Arc;

use frame::{AppDenyPolicy, Rect};

use crate::error::{BlockReason, FirewallError, PauseReason};
use crate::redacted_text::RedactedScreenText;

/// Maximum emitted (numbered) nodes.
pub const MAX_EMITTED_NODES: usize = 400;
/// Maximum characters of the whole screen text, including the final `[truncated]` marker.
pub const MAX_TEXT_CHARS: usize = 8_000;
/// Maximum characters of one label (longer labels are cut and end with `…`).
pub const MAX_LABEL_CHARS: usize = 120;
/// Maximum characters of a static-text / heading value (longer values are cut and end with `…`).
pub const MAX_VALUE_CHARS: usize = 400;
/// A leaf opaque node counts towards `opaque_area_permille` only when it covers at least this
/// percentage of the window area.
pub const OPAQUE_MIN_PERCENT: u64 = 1;
/// Marker line appended when a cap was hit.
pub const TRUNCATED_MARKER: &str = "[truncated]";

/// Role of an accessibility node, mapped by the platform source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Window,
    Button,
    Link,
    MenuItem,
    Tab,
    CheckBox,
    RadioButton,
    ComboBox,
    TextField,
    StaticText,
    Heading,
    Image,
    List,
    ListItem,
    Table,
    Toolbar,
    Group,
    WebArea,
    /// Content the tree cannot describe (canvas, plugin, custom-drawn view).
    Opaque,
    Other,
}

impl NodeRole {
    fn name(self) -> &'static str {
        match self {
            NodeRole::Window => "window",
            NodeRole::Button => "button",
            NodeRole::Link => "link",
            NodeRole::MenuItem => "menu item",
            NodeRole::Tab => "tab",
            NodeRole::CheckBox => "checkbox",
            NodeRole::RadioButton => "radio button",
            NodeRole::ComboBox => "combo box",
            NodeRole::TextField => "text field",
            NodeRole::StaticText => "text",
            NodeRole::Heading => "heading",
            NodeRole::Image => "image",
            NodeRole::List => "list",
            NodeRole::ListItem => "list item",
            NodeRole::Table => "table",
            NodeRole::Toolbar => "toolbar",
            NodeRole::Group => "group",
            NodeRole::WebArea => "web area",
            NodeRole::Opaque => "opaque",
            NodeRole::Other => "element",
        }
    }

    /// Roles whose label is a window/page title: dropped, never emitted (user decision).
    fn label_is_title(self) -> bool {
        matches!(self, NodeRole::Window | NodeRole::WebArea)
    }

    /// Roles that are worth a line even without a label (something you can act on).
    fn emit_unlabeled(self) -> bool {
        matches!(
            self,
            NodeRole::Button
                | NodeRole::Link
                | NodeRole::MenuItem
                | NodeRole::Tab
                | NodeRole::CheckBox
                | NodeRole::RadioButton
                | NodeRole::ComboBox
                | NodeRole::TextField
        )
    }

    /// Leaf nodes of these roles count as "opaque" screen area in the coverage facts.
    fn is_opaque_kind(self) -> bool {
        matches!(
            self,
            NodeRole::Image
                | NodeRole::Opaque
                | NodeRole::Group
                | NodeRole::WebArea
                | NodeRole::Other
        )
    }
}

/// One accessibility node as read by a platform source (untrusted input to `redact_tree`).
///
/// `Debug` prints depth and role only; never content.
#[derive(Clone)]
pub struct RawTextNode {
    /// Depth below the window root (root = 0); nodes are in DFS pre-order.
    pub depth: u16,
    pub role: NodeRole,
    /// Secure (password) field. Sources MUST decide this BEFORE reading any value.
    pub secure: bool,
    pub editable: bool,
    pub label: Option<String>,
    /// MUST be `None` when `secure` (never read). Only used for static text / headings; an
    /// editable field's value is never emitted.
    pub value: Option<String>,
    /// The node has a non-empty value (so `[has text]` can be shown without reading it).
    pub has_value: bool,
    /// Same global coordinate space as `SecureFieldDetector`.
    pub rect: Option<Rect>,
}

impl fmt::Debug for RawTextNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RawTextNode(depth {}, {:?}{})",
            self.depth,
            self.role,
            if self.secure { ", secure" } else { "" }
        )
    }
}

/// The tree of exactly ONE window: the focused window of the frontmost app. There is no way to
/// express several windows, which is deliberate: other windows are never read.
#[derive(Clone)]
pub struct RawWindowTree {
    /// Bundle id / executable name. Never a window title.
    pub app_id: String,
    pub window_rect: Rect,
    /// DFS pre-order; the first node is normally the window root (`depth 0`, `Window`).
    pub nodes: Vec<RawTextNode>,
}

impl fmt::Debug for RawWindowTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawWindowTree({} nodes)", self.nodes.len())
    }
}

/// Reads the accessibility tree of the foreground window.
///
/// # Contract (platform sources, T13)
/// - Read ONLY the focused window of the frontmost app. Never walk other windows or apps.
/// - Bounded walk (20 000 nodes, depth 64); beyond that return `Err`, never a partial tree.
/// - Decide whether a node is secure BEFORE reading any value; never read a secure node's value
///   or label; do not descend into secure nodes; never read window titles.
/// - Rects are in the same global space as `SecureFieldDetector`.
/// - Classify secure-ness exactly like the platform `SecureFieldDetector` (shared helpers).
///
/// The firewall re-enforces all of this on the returned data, but a source that reads secrets
/// into memory has already failed the contract.
pub trait ScreenTextSource: Send + Sync {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError>;
}

/// Source for platforms without a tree reader: always `Err(Unsupported)`, so `check_text`
/// blocks and turns are voice-only.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedTextSource;

impl ScreenTextSource for UnsupportedTextSource {
    fn foreground_tree(&self) -> Result<RawWindowTree, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

/// Facts about how much of the window the text describes (computed before caps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCoverage {
    /// Nodes of the window (root excluded) that have a rect intersecting the window, secure
    /// nodes included, descendants of secure nodes excluded.
    pub visible_nodes: u32,
    /// Visible non-secure nodes with a non-empty label (titles do not count) or value.
    pub meaningful_nodes: u32,
    /// Numbered lines actually in the text.
    pub emitted_nodes: u32,
    pub truncated: bool,
    /// Clipped area of leaf Image/Opaque/Group/WebArea/Other nodes covering >= 1 % of the
    /// window, divided by the window area, in permille (cap 1000).
    pub opaque_area_permille: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithheldReason {
    DeniedApp,
}

/// Result of `Firewall::check_text`.
#[derive(Debug)]
pub enum TextVerdict {
    Clear(RedactedScreenText),
    Withheld(WithheldReason),
    Paused(PauseReason),
    Blocked(BlockReason),
}

fn is_hostile_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}'
            // bidi embeddings / overrides / isolates and marks
            | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            | '\u{200E}' | '\u{200F}' | '\u{061C}'
            // zero-width and BOM
            | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
}

/// Step 9: neutralise anything that could forge prompt structure. Control characters, line and
/// paragraph separators and bidi controls become spaces, `<`/`>` become `‹`/`›`, `"` becomes `'`
/// (labels are quoted), whitespace is collapsed and trimmed.
pub(crate) fn sanitise(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '<' => '\u{2039}',
            '>' => '\u{203A}',
            '"' => '\'',
            c if is_hostile_char(c) => ' ',
            c => c,
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sanitise, then cut to `max` chars (ending with `…` when cut).
fn clean_capped(s: &str, max: usize) -> String {
    let t = sanitise(s);
    if t.chars().count() <= max {
        return t;
    }
    let mut cut: String = t.chars().take(max).collect();
    cut = cut.trim_end().to_string();
    cut.push('…');
    cut
}

fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.intersect(outer) == Some(*inner)
}

fn normalised(mut v: Vec<Rect>) -> Vec<Rect> {
    v.sort_by_key(|r| (r.x, r.y, r.w, r.h));
    v.dedup();
    v
}

/// Steps 4-11 of ADR 0002 A2 (pure). `record_rects` are the secure-field rects of the preflight
/// record. Never returns anything derived from a secure node's content.
pub(crate) fn redact_tree(
    tree: RawWindowTree,
    record_rects: &[Rect],
    deny: &dyn AppDenyPolicy,
) -> TextVerdict {
    let RawWindowTree {
        app_id,
        window_rect: window,
        nodes,
    } = tree;

    // 4. Unknown or denied app: nothing is read further.
    let app = sanitise(&app_id);
    if app_id.trim().is_empty() || app.is_empty() || deny.is_denied(&app_id) {
        return TextVerdict::Withheld(WithheldReason::DeniedApp);
    }
    if window.is_empty() {
        return TextVerdict::Blocked(BlockReason::detection("window has no usable area"));
    }

    // 5. Empty tree: zero visible nodes besides the root.
    let has_root = nodes
        .first()
        .is_some_and(|n| n.depth == 0 && n.role == NodeRole::Window);
    let skip = usize::from(has_root);
    let visible_besides_root = nodes
        .iter()
        .skip(skip)
        .any(|n| n.rect.is_some_and(|r| r.intersects(&window)));
    if !visible_besides_root {
        return TextVerdict::Blocked(BlockReason::IncompleteTree { app: app_id });
    }

    // 6. Secure cross-check against the preflight record (both directions).
    let mut tree_secure = Vec::new();
    for n in nodes.iter().filter(|n| n.secure) {
        match n.rect {
            Some(r) if !r.is_empty() => tree_secure.push(r),
            _ => {
                return TextVerdict::Blocked(BlockReason::detection(
                    "secure field has no usable area",
                ))
            }
        }
    }
    let secure_hidden = tree_secure.len() as u32;
    let tree_secure = normalised(tree_secure);
    let expected = normalised(
        record_rects
            .iter()
            .copied()
            .filter(|r| contains(&window, r))
            .collect(),
    );
    if tree_secure != expected {
        return TextVerdict::Blocked(BlockReason::UnstableLayout);
    }

    // 7. Keep nodes with a rect intersecting the window; drop descendants of secure nodes.
    struct Kept {
        idx: usize,
        clipped: Rect,
    }
    let mut kept: Vec<Kept> = Vec::new();
    let mut secure_depth: Option<u16> = None;
    for (idx, n) in nodes.iter().enumerate() {
        if let Some(d) = secure_depth {
            if n.depth > d {
                continue;
            }
            secure_depth = None;
        }
        if n.secure {
            secure_depth = Some(n.depth);
        }
        if has_root && idx == 0 {
            continue;
        }
        let Some(clipped) = n.rect.and_then(|r| r.intersect(&window)) else {
            continue;
        };
        kept.push(Kept { idx, clipped });
    }

    // 11 (computed before caps, from the kept set).
    let window_area = window.w as u64 * window.h as u64;
    let mut meaningful = 0u32;
    let mut opaque_area = 0u64;
    for kn in &kept {
        let n = &nodes[kn.idx];
        if n.secure {
            continue;
        }
        let label_ok =
            !n.role.label_is_title() && n.label.as_deref().is_some_and(|l| !l.trim().is_empty());
        let value_ok = n.value.as_deref().is_some_and(|v| !v.trim().is_empty());
        if label_ok || value_ok {
            meaningful += 1;
        }
        let is_leaf = nodes
            .get(kn.idx + 1)
            .is_none_or(|next| next.depth <= n.depth);
        if is_leaf && n.role.is_opaque_kind() {
            let area = kn.clipped.w as u64 * kn.clipped.h as u64;
            if area * 100 >= window_area * OPAQUE_MIN_PERCENT {
                opaque_area += area;
            }
        }
    }
    let opaque_area_permille = (opaque_area.saturating_mul(1000) / window_area).min(1000) as u16;
    let visible_nodes = kept.len() as u32;

    // 8-10. Per-node redaction, sanitisation, caps.
    let header = format!("app: {app}\n");
    let budget = MAX_TEXT_CHARS - TRUNCATED_MARKER.chars().count() - 1;
    let mut text = header;
    let mut used = text.chars().count();
    let mut rects: Vec<Rect> = Vec::new();
    let mut truncated = false;
    for kn in &kept {
        let n = &nodes[kn.idx];
        let Some(body) = node_body(n) else {
            continue;
        };
        let line_len = format!("#{} ", rects.len() + 1).len() + body.chars().count() + 1;
        if rects.len() >= MAX_EMITTED_NODES || used + line_len > budget {
            truncated = true;
            break;
        }
        rects.push(kn.clipped);
        text.push_str(&format!("#{} {}\n", rects.len(), body));
        used += line_len;
    }
    if truncated {
        text.push_str(TRUNCATED_MARKER);
        text.push('\n');
    }

    let coverage = TextCoverage {
        visible_nodes,
        meaningful_nodes: meaningful,
        emitted_nodes: rects.len() as u32,
        truncated,
        opaque_area_permille,
    };
    TextVerdict::Clear(RedactedScreenText::from_tree(
        Arc::from(text),
        Arc::from(rects),
        window,
        coverage,
        secure_hidden,
    ))
}

/// The line body (after `#id `) for one node, or `None` if the node is skipped.
fn node_body(n: &RawTextNode) -> Option<String> {
    // Secure: label and value are discarded unread.
    if n.secure {
        return Some("password field [hidden]".to_string());
    }
    let name = n.role.name();
    let label = if n.role.label_is_title() {
        String::new()
    } else {
        n.label
            .as_deref()
            .map(|l| clean_capped(l, MAX_LABEL_CHARS))
            .unwrap_or_default()
    };
    let quoted = |l: &str| format!("\"{l}\"");
    if n.editable || n.role == NodeRole::TextField {
        // The value is never emitted, only whether there is one.
        let has_text = n.has_value || n.value.as_deref().is_some_and(|v| !v.trim().is_empty());
        let state = if has_text { "[has text]" } else { "[empty]" };
        return Some(if label.is_empty() {
            format!("{name} {state}")
        } else {
            format!("{name} {} {state}", quoted(&label))
        });
    }
    if matches!(n.role, NodeRole::StaticText | NodeRole::Heading) {
        let t = n
            .value
            .as_deref()
            .map(|v| clean_capped(v, MAX_VALUE_CHARS))
            .filter(|v| !v.is_empty())
            .unwrap_or(label);
        return (!t.is_empty()).then(|| format!("{name} {}", quoted(&t)));
    }
    if !label.is_empty() {
        return Some(format!("{name} {}", quoted(&label)));
    }
    n.role.emit_unlabeled().then(|| name.to_string())
}
