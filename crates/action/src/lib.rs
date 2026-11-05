//! The agent action vocabulary (ADR 0004 §7). Pure data, no OS or I/O dependencies.
//!
//! An [`Action`] can only be built through validated constructors, so an invalid one (empty or
//! hostile text, bad launch id, scroll repeat outside 1..=10) cannot exist. There is no
//! `Default` and no `Deserialize`: the engine plan is parsed by `policy` into plan steps, which
//! call these constructors.
//!
//! # Canonical bytes
//! Little-endian; `lp(x)` = u64 length ‖ bytes.
//! `Action::canonical_bytes` = `0x01` ‖ tag (Focus 1, Scroll 2, Click 3, SetText 4, TypeText 5,
//! KeyCombo 6, CopySelection 7, OpenApp 8, FocusApp 9) ‖ fields in declaration order.
//! `ElementRef` = 24-byte `ObservationId::canonical` ‖ u32 node; `ScrollTarget` = u8 kind
//! (1 Element, 2 Window) ‖ payload (ElementRef, or 24-byte id); enums = u8 discriminant;
//! `TypedText` / `LaunchId` / `AppId` = `lp(UTF-8)` exactly, not normalised; scroll `repeat` = u8.
//!
//! # Digest
//! [`action_digest`] = `sha256("hey_buddy/action/v1"` (19 bytes, no length prefix) ‖
//! `lp(session_nonce)` ‖ `u64 task` ‖ `plan_digest` (32 B) ‖ `u16 step` ‖
//! `lp(action.canonical_bytes())` ‖ `lp(descriptor.canonical_bytes() or empty)`).
//! This single function reconciles ADR 0003 §6 (session/task/plan/step binding) with ADR 0004 §7
//! (action + descriptor). The driver re-checks `descriptor == obs.describe(target)`.

//!
//! # Invalid values cannot be built
//! ```compile_fail,E0423
//! // Tuple struct with a private field: only `TypedText::new` can make one.
//! let _ = action::TypedText(String::from("x"));
//! ```
//! ```compile_fail,E0451
//! // `Action` has a private field: no struct literal from outside the crate.
//! fn f(k: action::ActionKind) -> action::Action { action::Action { kind: k } }
//! ```
//! ```compile_fail,E0277
//! // No `Default`.
//! fn needs_default<T: Default>() {}
//! needs_default::<action::Action>();
//! ```
//! ```compile_fail,E0277
//! // No `Default` for `TypedText` either.
//! fn needs_default<T: Default>() {}
//! needs_default::<action::TypedText>();
//! ```
//! ```compile_fail,E0599
//! // No `Display` of text content.
//! let t = action::TypedText::new("x").unwrap();
//! let _ = t.to_string();
//! ```

mod error;
mod types;

use firewall::{ElementRef, ObservationId, TargetDescriptor};
use sha2::{Digest, Sha256};

pub use error::ActionError;
pub use firewall::AppId;
pub use types::{
    KeyCombo, LaunchId, ScrollAmount, ScrollDirection, TypedText, MAX_LAUNCH_ID_CHARS,
    MAX_TYPED_TEXT_CHARS,
};

pub const MAX_SCROLL_REPEAT: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollTarget {
    Element(ElementRef),
    Window(ObservationId),
}

/// The validated payload of an [`Action`]. Matching only: it cannot be turned into an `Action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    Focus {
        target: ElementRef,
    },
    Scroll {
        target: ScrollTarget,
        direction: ScrollDirection,
        amount: ScrollAmount,
        repeat: u8,
    },
    Click {
        target: ElementRef,
    },
    SetText {
        target: ElementRef,
        text: TypedText,
    },
    TypeText {
        target: ElementRef,
        text: TypedText,
    },
    KeyCombo {
        target: ElementRef,
        combo: KeyCombo,
    },
    CopySelection {
        target: ElementRef,
    },
    OpenApp {
        app: LaunchId,
    },
    FocusApp {
        app: AppId,
    },
}

/// Coarse class used by policy (a fact about the action, not a tier decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    /// Focus, Scroll, FocusApp: no content change.
    Navigate,
    /// Click.
    Click,
    /// SetText, TypeText.
    TextEntry,
    /// KeyCombo.
    Key,
    /// CopySelection.
    Clipboard,
    /// OpenApp.
    Launch,
}

/// A validated agent action. Construct with the associated functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    kind: ActionKind,
}

impl Action {
    pub fn focus(target: ElementRef) -> Self {
        Self {
            kind: ActionKind::Focus { target },
        }
    }

    pub fn scroll(
        target: ScrollTarget,
        direction: ScrollDirection,
        amount: ScrollAmount,
        repeat: u8,
    ) -> Result<Self, ActionError> {
        if !(1..=MAX_SCROLL_REPEAT).contains(&repeat) {
            return Err(ActionError::ScrollRepeatRange);
        }
        Ok(Self {
            kind: ActionKind::Scroll {
                target,
                direction,
                amount,
                repeat,
            },
        })
    }

    pub fn click(target: ElementRef) -> Self {
        Self {
            kind: ActionKind::Click { target },
        }
    }

    pub fn set_text(target: ElementRef, text: TypedText) -> Self {
        Self {
            kind: ActionKind::SetText { target, text },
        }
    }

    pub fn type_text(target: ElementRef, text: TypedText) -> Self {
        Self {
            kind: ActionKind::TypeText { target, text },
        }
    }

    pub fn key_combo(target: ElementRef, combo: KeyCombo) -> Self {
        Self {
            kind: ActionKind::KeyCombo { target, combo },
        }
    }

    pub fn copy_selection(target: ElementRef) -> Self {
        Self {
            kind: ActionKind::CopySelection { target },
        }
    }

    pub fn open_app(app: LaunchId) -> Self {
        Self {
            kind: ActionKind::OpenApp { app },
        }
    }

    pub fn focus_app(app: AppId) -> Self {
        Self {
            kind: ActionKind::FocusApp { app },
        }
    }

    pub fn kind(&self) -> &ActionKind {
        &self.kind
    }

    /// The element this action targets (`None` for Window scroll, OpenApp, FocusApp).
    pub fn target(&self) -> Option<ElementRef> {
        match &self.kind {
            ActionKind::Focus { target }
            | ActionKind::Click { target }
            | ActionKind::SetText { target, .. }
            | ActionKind::TypeText { target, .. }
            | ActionKind::KeyCombo { target, .. }
            | ActionKind::CopySelection { target } => Some(*target),
            ActionKind::Scroll {
                target: ScrollTarget::Element(e),
                ..
            } => Some(*e),
            ActionKind::Scroll { .. }
            | ActionKind::OpenApp { .. }
            | ActionKind::FocusApp { .. } => None,
        }
    }

    /// Stable token for audit, logs and tier lookup.
    pub fn op_name(&self) -> &'static str {
        match &self.kind {
            ActionKind::Focus { .. } => "focus",
            ActionKind::Scroll { .. } => "scroll",
            ActionKind::Click { .. } => "click",
            ActionKind::SetText { .. } => "set_text",
            ActionKind::TypeText { .. } => "type_text",
            ActionKind::KeyCombo { .. } => "key_combo",
            ActionKind::CopySelection { .. } => "copy_selection",
            ActionKind::OpenApp { .. } => "open_app",
            ActionKind::FocusApp { .. } => "focus_app",
        }
    }

    /// Character count of the text, for SetText / TypeText.
    pub fn text_len(&self) -> Option<u32> {
        match &self.kind {
            ActionKind::SetText { text, .. } | ActionKind::TypeText { text, .. } => {
                Some(text.char_len())
            }
            _ => None,
        }
    }

    /// `KeyCombo::Enter` (may submit a form).
    pub fn is_submit_like(&self) -> bool {
        matches!(
            &self.kind,
            ActionKind::KeyCombo {
                combo: KeyCombo::Enter,
                ..
            }
        )
    }

    pub fn kind_class(&self) -> ActionClass {
        match &self.kind {
            ActionKind::Focus { .. } | ActionKind::Scroll { .. } | ActionKind::FocusApp { .. } => {
                ActionClass::Navigate
            }
            ActionKind::Click { .. } => ActionClass::Click,
            ActionKind::SetText { .. } | ActionKind::TypeText { .. } => ActionClass::TextEntry,
            ActionKind::KeyCombo { .. } => ActionClass::Key,
            ActionKind::CopySelection { .. } => ActionClass::Clipboard,
            ActionKind::OpenApp { .. } => ActionClass::Launch,
        }
    }

    /// See the module docs for the exact layout.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![0x01];
        match &self.kind {
            ActionKind::Focus { target } => {
                out.push(1);
                out.extend_from_slice(&target.canonical());
            }
            ActionKind::Scroll {
                target,
                direction,
                amount,
                repeat,
            } => {
                out.push(2);
                match target {
                    ScrollTarget::Element(e) => {
                        out.push(1);
                        out.extend_from_slice(&e.canonical());
                    }
                    ScrollTarget::Window(id) => {
                        out.push(2);
                        out.extend_from_slice(&id.canonical());
                    }
                }
                out.push(*direction as u8);
                out.push(*amount as u8);
                out.push(*repeat);
            }
            ActionKind::Click { target } => {
                out.push(3);
                out.extend_from_slice(&target.canonical());
            }
            ActionKind::SetText { target, text } => {
                out.push(4);
                out.extend_from_slice(&target.canonical());
                lp(&mut out, text.as_str().as_bytes());
            }
            ActionKind::TypeText { target, text } => {
                out.push(5);
                out.extend_from_slice(&target.canonical());
                lp(&mut out, text.as_str().as_bytes());
            }
            ActionKind::KeyCombo { target, combo } => {
                out.push(6);
                out.extend_from_slice(&target.canonical());
                out.push(*combo as u8);
            }
            ActionKind::CopySelection { target } => {
                out.push(7);
                out.extend_from_slice(&target.canonical());
            }
            ActionKind::OpenApp { app } => {
                out.push(8);
                lp(&mut out, app.as_str().as_bytes());
            }
            ActionKind::FocusApp { app } => {
                out.push(9);
                lp(&mut out, app.as_str().as_bytes());
            }
        }
        out
    }
}

fn lp(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}

/// Digest the user approves and the driver re-checks. Layout in the module docs.
pub fn action_digest(
    session_nonce: &[u8; 32],
    task: u64,
    plan_digest: &[u8; 32],
    step: u16,
    action: &Action,
    descriptor: Option<&TargetDescriptor>,
) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"hey_buddy/action/v1");
    lp(&mut buf, session_nonce);
    buf.extend_from_slice(&task.to_le_bytes());
    buf.extend_from_slice(plan_digest);
    buf.extend_from_slice(&step.to_le_bytes());
    lp(&mut buf, &action.canonical_bytes());
    let d = descriptor
        .map(TargetDescriptor::canonical_bytes)
        .unwrap_or_default();
    lp(&mut buf, &d);
    Sha256::digest(&buf).into()
}
