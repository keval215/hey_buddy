//! `ProposedPlan` and the dry-run report (ADR 0003 sections 6 and 8). Both are sealed: only the
//! gate builds them, so a caller cannot forge a lower classification into a plan approval.

use std::fmt;

use firewall::{AppId, NodeRole};

use crate::bind::{BindError, BoundStep};
use crate::config::TaskOrigin;
use crate::ids::{hex, TaskId};
use crate::plan::{role_code, PlanStep, ValidatedPlan};
use crate::tier::{Classification, Tier};

/// A validated plan plus a PROVISIONAL classification per step (display only: assumes a benign
/// environment and an unknown target role; never used to authorise anything). Sealed.
pub struct ProposedPlan {
    pub(crate) id: u64,
    /// Id of the gate that proposed it: any other gate refuses it.
    pub(crate) gate: u64,
    pub(crate) task: TaskId,
    pub(crate) origin: TaskOrigin,
    pub(crate) plan: ValidatedPlan,
    pub(crate) provisional: Vec<Classification>,
}

impl ProposedPlan {
    pub fn task(&self) -> TaskId {
        self.task
    }
    pub fn origin(&self) -> TaskOrigin {
        self.origin
    }
    pub fn plan(&self) -> &ValidatedPlan {
        &self.plan
    }
    pub fn steps(&self) -> &[PlanStep] {
        self.plan.steps()
    }
    pub fn len(&self) -> usize {
        self.plan.len()
    }
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }
    /// Provisional per-step classification (display only).
    pub fn provisional(&self) -> &[Classification] {
        &self.provisional
    }
}

impl fmt::Debug for ProposedPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProposedPlan({:?}, {:?})", self.task, self.plan)
    }
}

/// What one step bound to in the dry run: firewall-cleared facts only. Sealed (no public
/// constructor). `Debug` prints no label or app id.
#[derive(Clone, PartialEq, Eq)]
pub struct DryBound {
    pub(crate) has_element: bool,
    pub(crate) node: u32,
    pub(crate) role: NodeRole,
    pub(crate) label: String,
    pub(crate) label_sha: [u8; 32],
    pub(crate) editable: bool,
    pub(crate) rect: (i32, i32, u32, u32),
    pub(crate) app: AppId,
    pub(crate) payment: bool,
    pub(crate) classification: Classification,
}

impl DryBound {
    /// Snapshot of a bound step. Steps without an element (scroll, open_app, key chords...) keep
    /// only the app, payment flag and tier (`has_element == false`).
    pub(crate) fn of(s: &BoundStep, classification: Classification) -> Self {
        let app = s
            .facts()
            .app_id
            .clone()
            .or_else(|| s.descriptor().map(|d| d.app_id.clone()))
            .unwrap_or_else(|| AppId::parse("unknown").expect("valid app id"));
        match s.descriptor() {
            Some(d) => Self {
                has_element: true,
                node: d.element.node,
                role: d.role,
                label: d.label.as_deref().unwrap_or_default().to_owned(),
                label_sha: d.label_sha256,
                editable: d.editable,
                rect: (d.rect.x, d.rect.y, d.rect.w, d.rect.h),
                app,
                payment: s.payment_context(),
                classification,
            },
            None => Self {
                has_element: false,
                node: 0,
                role: NodeRole::Window,
                label: String::new(),
                label_sha: [0; 32],
                editable: false,
                rect: (0, 0, 0, 0),
                app,
                payment: s.payment_context(),
                classification,
            },
        }
    }

    pub fn classification(&self) -> Classification {
        self.classification
    }
    pub fn role(&self) -> NodeRole {
        self.role
    }
    /// Firewall-sanitised label (render as quoted text, cap 60).
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn app_id(&self) -> &AppId {
        &self.app
    }
    pub fn editable(&self) -> bool {
        self.editable
    }
    pub fn payment_context(&self) -> bool {
        self.payment
    }
    /// `(x, y, w, h)` of the target; zeros for steps without an element.
    pub fn rect(&self) -> (i32, i32, u32, u32) {
        self.rect
    }
    /// The `#n` of the element in the observation text; 0 for steps without an element.
    pub fn node(&self) -> u32 {
        self.node
    }

    pub(crate) fn canonical(&self, out: &mut Vec<u8>) {
        out.push(1);
        out.push(u8::from(self.has_element));
        out.extend_from_slice(&self.node.to_le_bytes());
        out.push(role_code(Some(self.role)));
        out.extend_from_slice(&self.label_sha);
        out.extend_from_slice(&self.rect.0.to_le_bytes());
        out.extend_from_slice(&self.rect.1.to_le_bytes());
        out.extend_from_slice(&self.rect.2.to_le_bytes());
        out.extend_from_slice(&self.rect.3.to_le_bytes());
        out.push(u8::from(self.editable));
        out.push(u8::from(self.payment));
        let a = self.app.as_str().as_bytes();
        out.extend_from_slice(&(a.len() as u64).to_le_bytes());
        out.extend_from_slice(a);
        out.push(tier_code(self.classification.tier));
        out.push(u8::from(self.classification.submit));
        out.push(u8::from(self.classification.batchable));
    }
}

impl fmt::Debug for DryBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DryBound(#{}, {:?}, {})",
            self.node,
            self.role,
            self.classification.tier.as_str()
        )
    }
}

fn tier_code(t: Tier) -> u8 {
    match t {
        Tier::Read => 1,
        Tier::Reversible => 2,
        Tier::Consequential => 3,
        Tier::Never => 4,
    }
}

/// One step of a dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRunStep {
    Bound(DryBound),
    /// The step could not be bound against the current observation (for example the target
    /// appears only after an earlier step opens an app). It is asked again when it runs.
    Unbound(BindError),
}

/// Result of `PolicyGate::dry_run`: nothing executed, nothing minted. Sealed. Its canonical bytes
/// are part of the plan digest, so the user approves exactly these bindings.
pub struct DryRunReport {
    pub(crate) proposal: u64,
    /// Id of the gate that produced it: any other gate refuses it.
    pub(crate) gate: u64,
    pub(crate) steps: Vec<DryRunStep>,
    pub(crate) canonical: Vec<u8>,
}

impl DryRunReport {
    pub(crate) fn new(proposal: u64, gate: u64, steps: Vec<DryRunStep>) -> Self {
        let mut c = vec![0x01];
        c.extend_from_slice(&(steps.len() as u16).to_le_bytes());
        for s in &steps {
            match s {
                DryRunStep::Bound(b) => b.canonical(&mut c),
                DryRunStep::Unbound(e) => {
                    c.push(0);
                    let code = e.code().as_bytes();
                    c.extend_from_slice(&(code.len() as u64).to_le_bytes());
                    c.extend_from_slice(code);
                }
            }
        }
        Self {
            proposal,
            gate,
            steps,
            canonical: c,
        }
    }

    pub fn steps(&self) -> &[DryRunStep] {
        &self.steps
    }
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
    /// How many steps could not be bound.
    pub fn unbound(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, DryRunStep::Unbound(_)))
            .count()
    }
}

impl fmt::Debug for DryRunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DryRunReport({} steps, {} unbound, {})",
            self.steps.len(),
            self.unbound(),
            hex(&self.canonical[..self.canonical.len().min(4)])
        )
    }
}

/// Stable lowercase role token for the audit log (the plan/v1 spelling).
pub(crate) fn role_str(r: NodeRole) -> &'static str {
    match r {
        NodeRole::Window => "window",
        NodeRole::Button => "button",
        NodeRole::Link => "link",
        NodeRole::MenuItem => "menu_item",
        NodeRole::Tab => "tab",
        NodeRole::CheckBox => "checkbox",
        NodeRole::RadioButton => "radio_button",
        NodeRole::ComboBox => "combo_box",
        NodeRole::TextField => "text_field",
        NodeRole::StaticText => "static_text",
        NodeRole::Heading => "heading",
        NodeRole::Image => "image",
        NodeRole::List => "list",
        NodeRole::ListItem => "list_item",
        NodeRole::Table => "table",
        NodeRole::Toolbar => "toolbar",
        NodeRole::Group => "group",
        NodeRole::WebArea => "web_area",
        NodeRole::Opaque => "opaque",
        NodeRole::Other => "other",
    }
}
