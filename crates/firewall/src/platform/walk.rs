//! Bounded, secure-first tree walker shared by the macOS AX and Windows UIA text sources.
//! Pure and generic over a [`TreeCursor`], so the privacy-critical ordering is unit-tested on
//! Linux with a recording fake; the platform modules only implement the cursor.
//!
//! Guarantees (ADR 0002 A1/A2):
//! - Only the subtree below the root handed in is visited (the platform picks the foreground
//!   window; the walker never sees any other window).
//! - For every node `is_secure` is asked FIRST. A secure node: only its rect is read, it is
//!   reported with no label/value, and its children are never requested.
//! - The window root's label is never read; neither is the label of a `WebArea`, a `Tab`, a
//!   nested `Window` (UIA TitleBar), or a Group/Other at depth <= 2 (titles, D-003). Neither is
//!   the text value of such nodes (Document / web area value = the URL).
//! - Browser chrome (S2-5): Tab / nested Window nodes, address-bar-like editable nodes, tab
//!   strips, bookmark bars and download shelves are dropped WITH their subtree. Below a dropped
//!   node only `is_secure` (first) and `children` are asked, plus the rect of secure nodes. The
//!   label of an editable node is read only to decide this and is discarded when dropped.
//! - Editable fields: only `has_value` (emptiness) is asked, never their text.
//! - More than `max_nodes` nodes, deeper than `max_depth`, or past the deadline => `Err`.
// Used by the macOS/Windows sources and tests only.
#![cfg_attr(
    not(any(test, target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]

use std::time::{Duration, Instant};

use frame::Rect;

use crate::target::{hash_label, NodeIdentity};
use crate::text::{drops_subtree, has_body, MAX_EMITTED_NODES};
use crate::{FirewallError, NodeRole, RawTextNode, RawWindowTree};

pub(crate) const MAX_NODES: usize = 20_000;
pub(crate) const MAX_DEPTH: usize = 64;
/// Containers at this depth or above (window = 0) never have their label read.
const TOP_LEVEL_DEPTH: usize = 2;
/// Wall-clock budget of one text read, inside the firewall's 1500 ms text timeout.
pub(crate) const WALK_BUDGET: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub deadline: Instant,
}

impl Limits {
    pub(crate) fn standard(start: Instant) -> Self {
        Limits {
            max_nodes: MAX_NODES,
            max_depth: MAX_DEPTH,
            deadline: start + WALK_BUDGET,
        }
    }
}

/// Platform access to one tree. Every method reads exactly one thing, so the walker (and its
/// tests) control which attributes are ever touched.
pub(crate) trait TreeCursor {
    type Node;
    /// Role/subrole (AX) or `IsPassword` (UIA). Always asked before anything else.
    fn is_secure(&mut self, n: &Self::Node) -> Result<bool, FirewallError>;
    /// Global-space rect. `Ok(None)` = unknown. Invalid geometry may be `Err` (fail closed for
    /// secure nodes; treated as "no rect" for other nodes).
    fn rect(&mut self, n: &Self::Node) -> Result<Option<Rect>, FirewallError>;
    fn role(&mut self, n: &Self::Node) -> Result<NodeRole, FirewallError>;
    fn is_editable(&mut self, n: &Self::Node) -> Result<bool, FirewallError>;
    /// Emptiness ONLY: the implementation must drop the value immediately.
    fn has_value(&mut self, n: &Self::Node) -> Result<bool, FirewallError>;
    /// Element label (title/description/placeholder/name). Never called for window roots.
    fn label(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError>;
    /// Text of a static text / heading. Never called for editable or secure nodes.
    fn text_value(&mut self, n: &Self::Node) -> Result<Option<String>, FirewallError>;
    fn children(&mut self, n: &Self::Node) -> Result<Vec<Self::Node>, FirewallError>;
    /// Identity facts for `observe` (runtime id, control type, automation id hash). Called ONLY
    /// after `is_secure` returned false, and only for nodes that will be emitted (never for
    /// secure, dropped or title-ish nodes). It must not read the name: the walker hashes the
    /// label it has already read. Default: fail closed (the platform adapter implements it in
    /// S2-14d; until then `observe` is unsupported).
    #[allow(dead_code)]
    fn identity(&mut self, _n: &Self::Node) -> Result<NodeIdentity, FirewallError> {
        Err(FirewallError::Unsupported)
    }
}

fn fail() -> FirewallError {
    FirewallError::Detection
}

/// Walk the subtree of `root` (the focused window). Output is DFS pre-order. Never asks for
/// identity.
pub(crate) fn walk_tree<C: TreeCursor>(
    c: &mut C,
    root: C::Node,
    app_id: String,
    limits: &Limits,
) -> Result<RawWindowTree, FirewallError> {
    walk_inner(c, root, app_id, limits, false).map(|(t, _)| t)
}

/// Like [`walk_tree`], plus identity for emitted non-secure nodes (parallel to `nodes`).
#[allow(dead_code)] // used by the Windows adapter in S2-14d
pub(crate) fn walk_tree_identified<C: TreeCursor>(
    c: &mut C,
    root: C::Node,
    app_id: String,
    limits: &Limits,
) -> Result<(RawWindowTree, Vec<Option<NodeIdentity>>), FirewallError> {
    walk_inner(c, root, app_id, limits, true)
}

fn walk_inner<C: TreeCursor>(
    c: &mut C,
    root: C::Node,
    app_id: String,
    limits: &Limits,
    want_identity: bool,
) -> Result<(RawWindowTree, Vec<Option<NodeIdentity>>), FirewallError> {
    let mut identities: Vec<Option<NodeIdentity>> = Vec::new();
    let mut identity_reads = 0usize;
    let mut nodes: Vec<RawTextNode> = Vec::new();
    let mut window_rect: Option<Rect> = None;
    // (node, depth, below a dropped browser-chrome node)
    let mut stack: Vec<(C::Node, usize, bool)> = vec![(root, 0, false)];
    let mut visited = 0usize;

    while let Some((n, depth, dropped)) = stack.pop() {
        visited += 1;
        if visited > limits.max_nodes
            || depth > limits.max_depth
            || Instant::now() >= limits.deadline
        {
            return Err(fail());
        }

        // 1. Secure first: nothing else is read before this answer.
        if c.is_secure(&n)? {
            if depth == 0 {
                return Err(fail()); // a window is never a secure field
            }
            let rect = match c.rect(&n)? {
                Some(r) if !r.is_empty() => r,
                _ => return Err(fail()), // cannot be hidden if its area is unknown
            };
            nodes.push(RawTextNode {
                depth: depth as u16,
                role: NodeRole::TextField,
                secure: true,
                editable: true,
                label: None,
                value: None,
                has_value: false,
                rect: Some(rect),
            });
            identities.push(None);
            continue; // never descend into a secure node
        }

        let kids_of_dropped = |c: &mut C,
                               n: &C::Node,
                               stack: &mut Vec<(C::Node, usize, bool)>,
                               visited: usize|
         -> Result<(), FirewallError> {
            let kids = c.children(n)?;
            let remaining = limits.max_nodes.saturating_sub(visited + stack.len());
            if kids.len() > remaining {
                return Err(fail());
            }
            for k in kids.into_iter().rev() {
                stack.push((k, depth + 1, true));
            }
            Ok(())
        };
        if dropped {
            // Below dropped browser chrome: nothing is read except the children.
            kids_of_dropped(c, &n, &mut stack, visited)?;
            continue;
        }

        let rect = if depth == 0 {
            let r = c.rect(&n)?.filter(|r| !r.is_empty()).ok_or_else(fail)?;
            window_rect = Some(r);
            Some(r)
        } else {
            c.rect(&n).unwrap_or(None)
        };
        let role = if depth == 0 {
            NodeRole::Window
        } else {
            c.role(&n)?
        };
        // Tabs and nested windows (UIA TitleBar) are dropped with their subtree, unread.
        if depth > 0 && matches!(role, NodeRole::Window | NodeRole::Tab) {
            kids_of_dropped(c, &n, &mut stack, visited)?;
            continue;
        }
        let editable = c.is_editable(&n)?;
        // Labels that can carry a window/page title (D-003) are never read: window, web area,
        // tabs (other pages' titles), and top-level containers (depth <= 2), whose names in
        // Chromium/Electron/Safari often equal the page title.
        let titleish = matches!(role, NodeRole::Window | NodeRole::WebArea | NodeRole::Tab)
            || (depth <= TOP_LEVEL_DEPTH && matches!(role, NodeRole::Group | NodeRole::Other));
        let label = if titleish { None } else { c.label(&n)? };
        let mut node = RawTextNode {
            depth: depth as u16,
            role,
            secure: false,
            editable,
            label,
            value: None,
            has_value: false,
            rect,
        };
        // Address bars, tab strips, bookmark bars, download shelves: dropped before any value is
        // asked for (the label read above is discarded with the node).
        if depth > 0 && drops_subtree(&node) {
            kids_of_dropped(c, &n, &mut stack, visited)?;
            continue;
        }
        if editable || role == NodeRole::TextField {
            node.has_value = c.has_value(&n)?;
        } else if matches!(role, NodeRole::StaticText | NodeRole::Heading) {
            let v = c.text_value(&n)?;
            node.has_value = v.as_deref().is_some_and(|s| !s.trim().is_empty());
            node.value = v;
        }
        // Identity: only now (is_secure was false, not dropped, not title-ish), only for nodes
        // that will have a line, with a rect, and bounded by the emission cap. A failure makes
        // the node unaddressable, never the read fail.
        let ident = if want_identity
            && depth > 0
            && node.rect.is_some()
            && identity_reads < MAX_EMITTED_NODES
            && has_body(&node)
        {
            identity_reads += 1;
            c.identity(&n).ok().map(|mut i| {
                i.label_hash = hash_label(node.label.as_deref());
                i
            })
        } else {
            None
        };
        identities.push(ident);
        nodes.push(node);

        let kids = c.children(&n)?;
        // Cap before pushing: more children than the remaining node budget can never succeed.
        let remaining = limits.max_nodes.saturating_sub(visited + stack.len());
        if kids.len() > remaining {
            return Err(fail());
        }
        for k in kids.into_iter().rev() {
            stack.push((k, depth + 1, false));
        }
    }

    Ok((
        RawWindowTree {
            app_id,
            window_rect: window_rect.ok_or_else(fail)?,
            nodes,
        },
        identities,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FNode {
        secure: bool,
        role: NodeRole,
        editable: bool,
        label: Option<&'static str>,
        value: Option<&'static str>,
        rect: Option<Rect>,
        kids: Vec<usize>,
    }

    fn node(role: NodeRole, kids: Vec<usize>) -> FNode {
        FNode {
            secure: false,
            role,
            editable: false,
            label: Some("l"),
            value: Some("v"),
            rect: Some(Rect::new(0, 0, 10, 10)),
            kids,
        }
    }

    #[derive(Default)]
    struct Fake {
        nodes: Vec<FNode>,
        log: Vec<(usize, &'static str)>,
        sleep: Option<Duration>,
    }

    impl Fake {
        fn read(&self, n: usize, what: &'static str) -> bool {
            self.log.iter().any(|&(i, w)| i == n && w == what)
        }
        fn touched(&self, n: usize) -> bool {
            self.log.iter().any(|&(i, _)| i == n)
        }
    }

    impl TreeCursor for Fake {
        type Node = usize;
        fn is_secure(&mut self, n: &usize) -> Result<bool, FirewallError> {
            self.log.push((*n, "secure"));
            if let Some(d) = self.sleep {
                std::thread::sleep(d);
            }
            Ok(self.nodes[*n].secure)
        }
        fn rect(&mut self, n: &usize) -> Result<Option<Rect>, FirewallError> {
            self.log.push((*n, "rect"));
            Ok(self.nodes[*n].rect)
        }
        fn role(&mut self, n: &usize) -> Result<NodeRole, FirewallError> {
            self.log.push((*n, "role"));
            Ok(self.nodes[*n].role)
        }
        fn is_editable(&mut self, n: &usize) -> Result<bool, FirewallError> {
            self.log.push((*n, "editable"));
            Ok(self.nodes[*n].editable)
        }
        fn has_value(&mut self, n: &usize) -> Result<bool, FirewallError> {
            self.log.push((*n, "has_value"));
            Ok(self.nodes[*n].value.is_some())
        }
        fn label(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
            self.log.push((*n, "label"));
            Ok(self.nodes[*n].label.map(str::to_string))
        }
        fn text_value(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
            self.log.push((*n, "text_value"));
            Ok(self.nodes[*n].value.map(str::to_string))
        }
        fn children(&mut self, n: &usize) -> Result<Vec<usize>, FirewallError> {
            self.log.push((*n, "children"));
            Ok(self.nodes[*n].kids.clone())
        }
        fn identity(&mut self, n: &usize) -> Result<NodeIdentity, FirewallError> {
            self.log.push((*n, "identity"));
            Ok(NodeIdentity {
                runtime_id: vec![*n as i32],
                control_type: 7,
                automation_id_hash: [1; 32],
                label_hash: [0; 32],
            })
        }
    }

    fn limits() -> Limits {
        Limits::standard(Instant::now())
    }

    /// 0 window -> [1 group -> [2 text, 3 password], 4 button]; 5 = ANOTHER window, unreachable.
    fn sample() -> Fake {
        let mut text = node(NodeRole::StaticText, vec![]);
        text.value = Some("hello");
        let mut pw = node(NodeRole::TextField, vec![6]);
        pw.secure = true;
        let mut win = node(NodeRole::Window, vec![1, 4]);
        win.rect = Some(Rect::new(0, 0, 100, 100));
        Fake {
            nodes: vec![
                win,
                node(NodeRole::Group, vec![2, 3]),
                text,
                pw,
                node(NodeRole::Button, vec![]),
                node(NodeRole::Window, vec![]),
                node(NodeRole::StaticText, vec![]), // child of the password field
            ],
            ..Default::default()
        }
    }

    #[test]
    fn dfs_order_and_depths() {
        let mut f = sample();
        let t = walk_tree(&mut f, 0, "com.x".into(), &limits()).unwrap();
        let shape: Vec<(u16, NodeRole)> = t.nodes.iter().map(|n| (n.depth, n.role)).collect();
        assert_eq!(
            shape,
            vec![
                (0, NodeRole::Window),
                (1, NodeRole::Group),
                (2, NodeRole::StaticText),
                (2, NodeRole::TextField),
                (1, NodeRole::Button),
            ]
        );
        assert_eq!(t.app_id, "com.x");
        assert_eq!(t.window_rect, Rect::new(0, 0, 100, 100));
    }

    #[test]
    fn secure_is_asked_first_and_never_read_further() {
        let mut f = sample();
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        // First call for every node is is_secure.
        for i in [0usize, 1, 2, 3, 4] {
            assert_eq!(f.log.iter().find(|e| e.0 == i).unwrap().1, "secure");
        }
        // Secure node 3: only is_secure and rect.
        let what: Vec<_> = f.log.iter().filter(|e| e.0 == 3).map(|e| e.1).collect();
        assert_eq!(what, vec!["secure", "rect"]);
        assert!(!f.touched(6), "child of a secure node must not be visited");
        let s = t.nodes.iter().find(|n| n.secure).unwrap();
        assert!(s.label.is_none() && s.value.is_none() && !s.has_value);
        assert!(s.rect.is_some());
    }

    #[test]
    fn foreground_only() {
        let mut f = sample();
        walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.touched(5), "another window was touched");
    }

    #[test]
    fn window_label_and_webarea_label_never_read() {
        let mut f = sample();
        f.nodes[4] = node(NodeRole::WebArea, vec![]);
        walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.read(0, "label"));
        assert!(!f.read(4, "label"));
        assert!(f.read(2, "label"));
    }

    #[test]
    fn top_level_containers_and_tabs_never_have_labels_read() {
        // 0 window -> [1 group -> [2 group -> [3 group -> [4 group]]], 5 tab, 6 other]
        let mut f = Fake {
            nodes: vec![
                node(NodeRole::Window, vec![1, 5, 6]),
                node(NodeRole::Group, vec![2]),
                node(NodeRole::Group, vec![3]),
                node(NodeRole::Group, vec![4]),
                node(NodeRole::Group, vec![]),
                node(NodeRole::Tab, vec![]),
                node(NodeRole::Other, vec![]),
            ],
            ..Default::default()
        };
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.read(1, "label"), "depth 1 group");
        assert!(!f.read(2, "label"), "depth 2 group");
        assert!(f.read(3, "label"), "depth 3 group is still read");
        assert!(f.read(4, "label"));
        assert!(!f.read(5, "label"), "tab label");
        assert!(!f.read(6, "label"), "depth 1 other");
        assert!(t
            .nodes
            .iter()
            .filter(|n| n.depth <= 2)
            .all(|n| n.label.is_none()));
    }

    #[test]
    fn too_many_children_for_the_budget_is_an_error() {
        let mut nodes = vec![node(NodeRole::Window, (1..=10).collect())];
        for _ in 0..10 {
            nodes.push(node(NodeRole::Button, vec![]));
        }
        let mut f = Fake {
            nodes,
            ..Default::default()
        };
        let mut l = limits();
        l.max_nodes = 5;
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_err());
        assert!(!f.touched(1), "no child may be visited once the cap trips");
        l.max_nodes = 11; // exactly window + 10 children: fine
        let mut nodes = vec![node(NodeRole::Window, (1..=10).collect())];
        for _ in 0..10 {
            nodes.push(node(NodeRole::Button, vec![]));
        }
        let mut f = Fake {
            nodes,
            ..Default::default()
        };
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_ok());
    }

    #[test]
    fn editable_only_emptiness() {
        let mut f = sample();
        let mut e = node(NodeRole::TextField, vec![]);
        e.editable = true;
        e.value = Some("secret-ish");
        f.nodes[4] = e;
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.read(4, "text_value"));
        assert!(f.read(4, "has_value"));
        let n = &t.nodes[4];
        assert!(n.has_value && n.value.is_none() && n.editable);
    }

    #[test]
    fn editable_static_role_is_not_value_read() {
        // A node flagged editable with a StaticText role must still not have its text read.
        let mut f = sample();
        f.nodes[4] = node(NodeRole::StaticText, vec![]);
        f.nodes[4].editable = true;
        walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.read(4, "text_value"));
    }

    #[test]
    fn static_text_value_is_read() {
        let mut f = sample();
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert_eq!(t.nodes[2].value.as_deref(), Some("hello"));
        assert!(t.nodes[2].has_value);
    }

    #[test]
    fn node_bound_is_an_error() {
        let mut f = sample();
        let mut l = limits();
        l.max_nodes = 3;
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_err());
        let mut f = sample();
        l.max_nodes = 5; // exactly the 5 reachable nodes: fine
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_ok());
    }

    #[test]
    fn depth_bound_is_an_error() {
        let mut f = sample();
        let mut l = limits();
        l.max_depth = 1;
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_err());
        let mut f = sample();
        l.max_depth = 2;
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_ok());
    }

    #[test]
    fn deep_chain_over_64_is_an_error() {
        let mut nodes = vec![node(NodeRole::Window, vec![1])];
        for i in 1..=70 {
            nodes.push(node(NodeRole::Group, vec![i + 1]));
        }
        nodes.push(node(NodeRole::Group, vec![]));
        let mut f = Fake {
            nodes,
            ..Default::default()
        };
        assert!(walk_tree(&mut f, 0, "a".into(), &limits()).is_err());
    }

    #[test]
    fn time_bound_is_an_error() {
        let mut f = sample();
        let l = Limits {
            deadline: Instant::now(),
            ..limits()
        };
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_err());
        // Slow cursor crossing the deadline mid-walk.
        let mut f = sample();
        f.sleep = Some(Duration::from_millis(20));
        let l = Limits {
            deadline: Instant::now() + Duration::from_millis(30),
            ..limits()
        };
        assert!(walk_tree(&mut f, 0, "a".into(), &l).is_err());
    }

    #[test]
    fn secure_without_usable_rect_is_an_error() {
        for bad in [None, Some(Rect::new(0, 0, 0, 5))] {
            let mut f = sample();
            f.nodes[3].rect = bad;
            assert!(walk_tree(&mut f, 0, "a".into(), &limits()).is_err());
        }
    }

    #[test]
    fn secure_root_or_missing_window_rect_is_an_error() {
        let mut f = sample();
        f.nodes[0].secure = true;
        assert!(walk_tree(&mut f, 0, "a".into(), &limits()).is_err());
        let mut f = sample();
        f.nodes[0].rect = None;
        assert!(walk_tree(&mut f, 0, "a".into(), &limits()).is_err());
    }

    #[test]
    fn non_secure_without_rect_is_kept_rectless() {
        let mut f = sample();
        f.nodes[4].rect = None;
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(t.nodes[4].rect.is_none());
    }

    // ---- S2-5 ----

    #[test]
    fn titles_and_page_values_are_never_read_for_title_roles() {
        // 0 window -> [1 titlebar(Window) -> [2 text], 3 webarea -> [4 text], 4.. ]
        let mut f = Fake {
            nodes: vec![
                node(NodeRole::Window, vec![1, 3]),
                node(NodeRole::Window, vec![2]),
                node(NodeRole::StaticText, vec![]),
                node(NodeRole::WebArea, vec![4]),
                node(NodeRole::StaticText, vec![]),
            ],
            ..Default::default()
        };
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        for i in [0usize, 1, 3] {
            assert!(!f.read(i, "label"), "label of title node {i}");
            assert!(!f.read(i, "text_value"), "value of title node {i}");
            assert!(!f.read(i, "has_value"), "has_value of title node {i}");
        }
        // The title bar's children are dropped unread; page content is kept.
        let what: Vec<_> = f.log.iter().filter(|e| e.0 == 2).map(|e| e.1).collect();
        assert_eq!(what, vec!["secure", "children"]);
        assert!(f.read(4, "text_value"));
        let shape: Vec<NodeRole> = t.nodes.iter().map(|n| n.role).collect();
        assert_eq!(
            shape,
            vec![NodeRole::Window, NodeRole::WebArea, NodeRole::StaticText]
        );
        assert!(t.nodes.iter().all(|n| n.label.is_none() || n.depth > 0));
    }

    #[test]
    fn address_bar_tab_strip_and_shelves_are_dropped_unread_below_the_label() {
        // 0 win -> [1 group(depth1) -> [2 edit "Address and search bar" -> [3 text],
        //           4 toolbar "Bookmarks bar" -> [5 button], 6 tab -> [7 button],
        //           8 group "Tab strip" -> [9 button], 10 button "ok"]]
        let mut addr = node(NodeRole::TextField, vec![3]);
        addr.editable = true;
        addr.label = Some("Address and search bar");
        addr.value = Some("https://evil.example/?t=SECRET");
        let mut bm = node(NodeRole::Toolbar, vec![5]);
        bm.label = Some("Bookmarks bar");
        let mut strip = node(NodeRole::Group, vec![9]);
        strip.label = Some("Tab strip");
        let mut f = Fake {
            nodes: vec![
                node(NodeRole::Window, vec![1]),
                node(NodeRole::Group, vec![11]),
                addr,
                node(NodeRole::StaticText, vec![]),
                bm,
                node(NodeRole::Button, vec![]),
                node(NodeRole::Tab, vec![7]),
                node(NodeRole::Button, vec![]),
                strip,
                node(NodeRole::Button, vec![]),
                node(NodeRole::Button, vec![]),
                // 11: wrapper at depth 2 (labels of top-level groups are not read at all)
                node(NodeRole::Group, vec![2, 4, 6, 8, 10]),
            ],
            ..Default::default()
        };
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        // Dropped nodes never reach the output, nor do their descendants.
        let n_out = t.nodes.len();
        assert_eq!(n_out, 4, "window, group, wrapper, ok-button: {:?}", t.nodes);
        // The address bar value/emptiness is never asked for; descendants only get
        // is_secure + children.
        assert!(!f.read(2, "text_value") && !f.read(2, "has_value"));
        for d in [3usize, 5, 7, 9] {
            let what: Vec<_> = f.log.iter().filter(|e| e.0 == d).map(|e| e.1).collect();
            assert_eq!(what, vec!["secure", "children"], "descendant {d}");
        }
        // Secure is still asked first for every node.
        for i in 0..=11usize {
            assert_eq!(f.log.iter().find(|e| e.0 == i).unwrap().1, "secure");
        }
    }

    #[test]
    fn secure_nodes_below_dropped_chrome_are_still_reported_hidden() {
        // 0 win -> [1 tab -> [2 password]]
        let mut pw = node(NodeRole::TextField, vec![3]);
        pw.secure = true;
        let mut f = Fake {
            nodes: vec![
                node(NodeRole::Window, vec![1]),
                node(NodeRole::Tab, vec![2]),
                pw,
                node(NodeRole::StaticText, vec![]),
            ],
            ..Default::default()
        };
        let t = walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert_eq!(t.nodes.len(), 2);
        assert!(t.nodes[1].secure && t.nodes[1].label.is_none());
        assert!(!f.touched(3), "child of a secure node must not be visited");
    }

    // ---- S2-14a: identity ----

    fn first_of(f: &Fake, n: usize) -> &'static str {
        f.log.iter().find(|e| e.0 == n).unwrap().1
    }

    #[test]
    fn identity_is_never_asked_by_the_plain_walk() {
        let mut f = sample();
        walk_tree(&mut f, 0, "a".into(), &limits()).unwrap();
        assert!(!f.log.iter().any(|e| e.1 == "identity"));
    }

    #[test]
    fn identity_only_after_is_secure_false_and_only_for_emitted_nodes() {
        let mut f = sample();
        let (t, ids) = walk_tree_identified(&mut f, 0, "a".into(), &limits()).unwrap();
        assert_eq!(ids.len(), t.nodes.len());
        for i in 0..=4usize {
            assert_eq!(first_of(&f, i), "secure", "is_secure first for {i}");
        }
        // Secure node 3 and the child behind it: never any identity.
        assert!(!f.read(3, "identity") && !f.touched(6));
        // The window root and the title-ish top-level group have no line; text and button do.
        assert!(!f.read(0, "identity") && !f.read(1, "identity"));
        for i in [2usize, 4] {
            let pos_secure = f.log.iter().position(|e| *e == (i, "secure")).unwrap();
            let pos_ident = f.log.iter().position(|e| *e == (i, "identity")).unwrap();
            assert!(pos_secure < pos_ident);
        }
        assert!(
            ids[0].is_none() && ids[3].is_none(),
            "root and secure: no identity"
        );
        let id = ids[2].as_ref().unwrap();
        assert_eq!(id.runtime_id, vec![2]);
        assert_eq!(
            id.label_hash,
            hash_label(Some("l")),
            "walker hashes the label it read"
        );
    }

    #[test]
    fn identity_never_for_title_ish_or_dropped_nodes_and_labels_stay_unread() {
        // 0 window -> [1 webarea, 2 tab -> [3 button], 4 group(depth1), 5 nested window -> [6 text],
        //              7 edit "Address and search bar", 8 button]
        let mut addr = node(NodeRole::TextField, vec![]);
        addr.editable = true;
        addr.label = Some("Address and search bar");
        let mut f = Fake {
            nodes: vec![
                node(NodeRole::Window, vec![1, 2, 4, 5, 7, 8]),
                node(NodeRole::WebArea, vec![]),
                node(NodeRole::Tab, vec![3]),
                node(NodeRole::Button, vec![]),
                node(NodeRole::Group, vec![]),
                node(NodeRole::Window, vec![6]),
                node(NodeRole::StaticText, vec![]),
                addr,
                node(NodeRole::Button, vec![]),
            ],
            ..Default::default()
        };
        let (_t, ids) = walk_tree_identified(&mut f, 0, "a".into(), &limits()).unwrap();
        for i in [0usize, 1, 2, 3, 4, 5, 6, 7] {
            assert!(!f.read(i, "identity"), "identity read for node {i}");
        }
        assert!(f.read(8, "identity"));
        for i in [0usize, 1, 2, 4, 5] {
            assert!(!f.read(i, "label"), "label read for title-ish node {i}");
        }
        assert_eq!(ids.iter().flatten().count(), 1);
    }

    #[test]
    fn identity_failure_makes_the_node_unaddressable_not_the_read_fail() {
        struct NoIdent(Fake);
        impl TreeCursor for NoIdent {
            type Node = usize;
            fn is_secure(&mut self, n: &usize) -> Result<bool, FirewallError> {
                self.0.is_secure(n)
            }
            fn rect(&mut self, n: &usize) -> Result<Option<Rect>, FirewallError> {
                self.0.rect(n)
            }
            fn role(&mut self, n: &usize) -> Result<NodeRole, FirewallError> {
                self.0.role(n)
            }
            fn is_editable(&mut self, n: &usize) -> Result<bool, FirewallError> {
                self.0.is_editable(n)
            }
            fn has_value(&mut self, n: &usize) -> Result<bool, FirewallError> {
                self.0.has_value(n)
            }
            fn label(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
                self.0.label(n)
            }
            fn text_value(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
                self.0.text_value(n)
            }
            fn children(&mut self, n: &usize) -> Result<Vec<usize>, FirewallError> {
                self.0.children(n)
            }
            // identity: trait default = Err(Unsupported)
        }
        let mut c = NoIdent(sample());
        let (t, ids) = walk_tree_identified(&mut c, 0, "a".into(), &limits()).unwrap();
        assert_eq!(ids.len(), t.nodes.len());
        assert!(ids.iter().all(Option::is_none));
    }
}
