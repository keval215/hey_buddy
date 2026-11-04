//! The platform effectors. NO read methods by design: an actuator can push input and nothing
//! else, so it can never become a second reader of screen content (ADR 0004 section 4.6).

use std::path::Path;

use action::{KeyCombo, ScrollAmount, ScrollDirection, TypedText};
use firewall::{AppId, LiveTarget};

use crate::error::ActError;

/// How a click is delivered, by control role (UIA patterns only; never a synthetic mouse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMethod {
    /// Button / Link / MenuItem: `InvokePattern.Invoke`.
    Invoke,
    /// CheckBox: `TogglePattern.Toggle`.
    Toggle,
    /// RadioButton / Tab / ListItem: `SelectionItemPattern.Select`.
    Select,
    /// ComboBox: `ExpandCollapsePattern`.
    ExpandCollapse,
}

/// Maximum characters per `send_chars` call.
pub const MAX_CHUNK_CHARS: usize = 16;

/// Pushes input. Every method is called on the driver's deadline worker, only after the driver
/// checked `ApprovedAction::check_live`, the kill switch and the release generation, and (for the
/// target-taking ones) with a [`LiveTarget`] the firewall revalidated a moment earlier.
///
/// The `send_*` methods carry no target: the driver calls them only after
/// `LiveTarget::verify_focus`. Return values are never delivery proof.
pub trait Actuator: Send + Sync {
    fn invoke(&self, t: &LiveTarget<'_>, m: ClickMethod) -> Result<(), ActError>;
    /// `ValuePattern.SetValue` (replace); requires the element not to be read-only.
    fn set_value(&self, t: &LiveTarget<'_>, text: &TypedText) -> Result<(), ActError>;
    fn set_focus(&self, t: &LiveTarget<'_>) -> Result<(), ActError>;
    /// One scroll step (the driver loops for `repeat`).
    fn scroll(
        &self,
        t: &LiveTarget<'_>,
        d: ScrollDirection,
        a: ScrollAmount,
    ) -> Result<(), ActError>;
    /// At most [`MAX_CHUNK_CHARS`] characters (`KEYEVENTF_UNICODE`). Typed text appears nowhere
    /// else in the driver.
    fn send_chars(&self, chunk: &str) -> Result<(), ActError>;
    fn send_key(&self, k: KeyCombo) -> Result<(), ActError>;
    /// Ctrl+C. The clipboard is never read.
    fn send_copy(&self) -> Result<(), ActError>;
    /// `CreateProcessW(path, "\"path\"")`: no arguments, verbs or URIs.
    fn launch(&self, path: &Path) -> Result<(), ActError>;
    /// Bring the top-z visible window of `app` to the front. The driver checked that `app` is in
    /// the observation, not denied, not restricted and not ours.
    fn focus_app(&self, app: &AppId) -> Result<(), ActError>;
    /// Release every key and button this actuator itself pressed. Idempotent. Called by
    /// `Driver::emergency_release` and again after any input call that returned while the
    /// driver was released meanwhile, and after an actuator panic.
    fn release_all(&self) {}
}

/// Everything is `Err(Unsupported)` (Linux, macOS).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedActuator;

const NOPE: ActError = ActError::Unsupported("no actuator on this platform");

impl Actuator for UnsupportedActuator {
    fn invoke(&self, _t: &LiveTarget<'_>, _m: ClickMethod) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn set_value(&self, _t: &LiveTarget<'_>, _text: &TypedText) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn set_focus(&self, _t: &LiveTarget<'_>) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn scroll(
        &self,
        _t: &LiveTarget<'_>,
        _d: ScrollDirection,
        _a: ScrollAmount,
    ) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn send_chars(&self, _chunk: &str) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn send_key(&self, _k: KeyCombo) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn send_copy(&self) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn launch(&self, _path: &Path) -> Result<(), ActError> {
        Err(NOPE)
    }
    fn focus_app(&self, _app: &AppId) -> Result<(), ActError> {
        Err(NOPE)
    }
}
