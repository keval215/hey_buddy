//! Bounded, secure-first tree walker shared by the macOS AX and Windows UIA text sources.
//! Pure and generic over a [`TreeCursor`], so the privacy-critical ordering is unit-tested on
//! Linux with a recording fake; the platform modules only implement the cursor.
//!
//! Guarantees (ADR 0002 A1/A2):
//! - Only the subtree below the root handed in is visited (the platform picks the foreground
//!   window; the walker never sees any other window).
//! - For every node `is_secure` is asked FIRST. A secure node: only its rect is read, it is
//!   reported with no label/value, and its children are never requested.
//! - The window root's label is never read; neither is the label of a `WebArea`, a `Tab`, or a
//!   Group/Other at depth <= 2 (titles, D-003).
//! - Editable fields: only `has_value` (emptiness) is asked, never their text.
//! - More than `max_nodes` nodes, deeper than `max_depth`, or past the deadline => `Err`.
// Used by the macOS/Windows sources and tests only.
#![cfg_attr(
    not(any(test, target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]

use std::time::{Duration, Instant};

use frame::Rect;

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
}

fn fail() -> FirewallError {
    FirewallError::Detection
}

/// Walk the subtree of `root` (the focused window). Output is DFS pre-order.
pub(crate) fn walk_tree<C: TreeCursor>(
    c: &mut C,
    root: C::Node,
    app_id: String,
    limits: &Limits,
) -> Result<RawWindowTree, FirewallError> {
    let mut nodes: Vec<RawTextNode> = Vec::new();
    let mut window_rect: Option<Rect> = None;
    let mut stack: Vec<(C::Node, usize)> = vec![(root, 0)];
    let mut visited = 0usize;

    while let Some((n, depth)) = stack.pop() {
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
            continue; // never descend into a secure node
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
        let editable = c.is_editable(&n)?;
        // Labels that can carry a window/page title (D-003) are never read: window, web area,
        // tabs (other pages' titles), and top-level containers (depth <= 2), whose names in
        // Chromium/Electron/Safari often equal the page title.
        let titleish = matches!(role, NodeRole::Window | NodeRole::WebArea | NodeRole::Tab)
            || (depth <= TOP_LEVEL_DEPTH && matches!(role, NodeRole::Group | NodeRole::Other));
        let label = if titleish { None } else { c.label(&n)? };
        let (value, has_value) = if editable || role == NodeRole::TextField {
            (None, c.has_value(&n)?)
        } else if matches!(role, NodeRole::StaticText | NodeRole::Heading) {
            let v = c.text_value(&n)?;
            let has = v.as_deref().is_some_and(|s| !s.trim().is_empty());
            (v, has)
        } else {
            (None, false)
        };
        nodes.push(RawTextNode {
            depth: depth as u16,
            role,
            secure: false,
            editable,
            label,
            value,
            has_value,
            rect,
        });

        let kids = c.children(&n)?;
        // Cap before pushing: more children than the remaining node budget can never succeed.
        let remaining = limits.max_nodes.saturating_sub(visited + stack.len());
        if kids.len() > remaining {
            return Err(fail());
        }
        for k in kids.into_iter().rev() {
            stack.push((k, depth + 1));
        }
    }

    Ok(RawWindowTree {
        app_id,
        window_rect: window_rect.ok_or_else(fail)?,
        nodes,
    })
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
}
