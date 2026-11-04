//! Driver errors and outcomes. Fixed tokens only (L-010): never an OS string, label or typed text.

use std::fmt;

use firewall::{BlockReason, ObservationId, PauseReason, TargetRefusal};

/// Why the driver refused or stopped. `Debug` and `Display` are content-free by construction.
#[derive(thiserror::Error, Clone, PartialEq, Eq)]
pub enum DriverError {
    #[error("stale reference")]
    StaleRef,
    /// `field` is a fixed name: `app`, `control_type`, `automation_id`, `label`, `rect`.
    #[error("target changed: {field}")]
    TargetChanged { field: &'static str },
    #[error("descriptor mismatch")]
    DescriptorMismatch,
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
    #[error("key not allowed here")]
    KeyNotAllowedHere,
    #[error("launch not allowed")]
    LaunchNotAllowed,
    #[error("app not observed")]
    AppNotObserved,
    #[error("rate limited")]
    RateLimited,
    #[error("killed")]
    Killed,
    #[error("timeout")]
    Timeout,
    #[error("busy")]
    Busy,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("platform error: {0}")]
    Platform(&'static str),
    /// `ApprovedAction::check_live` failed (kill, void or expiry): nothing was sent.
    #[error("approval revoked")]
    Revoked,
}

impl DriverError {
    /// Stable fixed token for audit and logs (`policy::ExecResult::code`).
    pub fn code(&self) -> &'static str {
        match self {
            DriverError::StaleRef => "stale_ref",
            DriverError::TargetChanged { .. } => "target_changed",
            DriverError::DescriptorMismatch => "descriptor_mismatch",
            DriverError::DeniedApp => "denied_app",
            DriverError::RestrictedApp => "restricted_app",
            DriverError::AuthSurface => "auth_surface",
            DriverError::OwnWindow => "own_window",
            DriverError::ElevatedTarget => "elevated_target",
            DriverError::SecureTarget => "secure_target",
            DriverError::SensitiveTarget => "sensitive_target",
            DriverError::PaymentSuspected => "payment_suspected",
            DriverError::NotEditable => "not_editable",
            DriverError::SessionPaused(_) => "session_paused",
            DriverError::Blocked(_) => "blocked",
            DriverError::ForegroundChanged => "foreground_changed",
            DriverError::FocusLost => "focus_lost",
            DriverError::NoSemanticPath => "no_semantic_path",
            DriverError::KeyNotAllowedHere => "key_not_allowed_here",
            DriverError::LaunchNotAllowed => "launch_not_allowed",
            DriverError::AppNotObserved => "app_not_observed",
            DriverError::RateLimited => "rate_limited",
            DriverError::Killed => "killed",
            DriverError::Timeout => "timeout",
            DriverError::Busy => "busy",
            DriverError::Unsupported(_) => "unsupported",
            DriverError::Platform(_) => "platform",
            DriverError::Revoked => "revoked",
        }
    }

    /// The fixed sub-token (`TargetChanged` field, `Unsupported` / `Platform` reason), if any.
    pub fn detail(&self) -> Option<&'static str> {
        match self {
            DriverError::TargetChanged { field } => Some(field),
            DriverError::Unsupported(s) | DriverError::Platform(s) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Debug for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fixed code (and fixed sub-token) only: never the payload of a `BlockReason`.
        match self.detail() {
            Some(d) => write!(f, "DriverError({}: {d})", self.code()),
            None => write!(f, "DriverError({})", self.code()),
        }
    }
}

impl From<TargetRefusal> for DriverError {
    fn from(r: TargetRefusal) -> Self {
        match r {
            TargetRefusal::StaleRef => DriverError::StaleRef,
            TargetRefusal::TargetChanged { field } => DriverError::TargetChanged { field },
            TargetRefusal::DeniedApp => DriverError::DeniedApp,
            TargetRefusal::RestrictedApp => DriverError::RestrictedApp,
            TargetRefusal::AuthSurface => DriverError::AuthSurface,
            TargetRefusal::OwnWindow => DriverError::OwnWindow,
            TargetRefusal::ElevatedTarget => DriverError::ElevatedTarget,
            TargetRefusal::SecureTarget => DriverError::SecureTarget,
            TargetRefusal::SensitiveTarget => DriverError::SensitiveTarget,
            TargetRefusal::PaymentSuspected => DriverError::PaymentSuspected,
            TargetRefusal::NotEditable => DriverError::NotEditable,
            TargetRefusal::SessionPaused(p) => DriverError::SessionPaused(p),
            TargetRefusal::Blocked(b) => DriverError::Blocked(b),
            TargetRefusal::ForegroundChanged => DriverError::ForegroundChanged,
            TargetRefusal::FocusLost => DriverError::FocusLost,
            TargetRefusal::NoSemanticPath => DriverError::NoSemanticPath,
            TargetRefusal::Unsupported => DriverError::Unsupported("firewall"),
        }
    }
}

/// What an [`Actuator`](crate::Actuator) call reports. Fixed tokens only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActError {
    /// The platform cannot do this (nothing was sent).
    Unsupported(&'static str),
    /// The call failed; whether it had an effect is unknown.
    Platform(&'static str),
}

impl fmt::Debug for ActError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActError::Unsupported(s) => write!(f, "ActError(unsupported: {s})"),
            ActError::Platform(s) => write!(f, "ActError(platform: {s})"),
        }
    }
}

impl From<ActError> for DriverError {
    fn from(e: ActError) -> Self {
        match e {
            ActError::Unsupported(s) => DriverError::Unsupported(s),
            ActError::Platform(s) => DriverError::Platform(s),
        }
    }
}

/// What reached the target. `SendInput` / pattern return values are never delivery proof, so a
/// success is only ever `DeliveredUnverified` (the next observation shows the effect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    NotDelivered,
    DeliveredUnverified,
    /// Typing stopped after `chars_sent` characters (never undone).
    Partial {
        chars_sent: u32,
    },
}

impl Delivery {
    pub fn code(&self) -> &'static str {
        match self {
            Delivery::NotDelivered => "not_delivered",
            Delivery::DeliveredUnverified => "delivered_unverified",
            Delivery::Partial { .. } => "partial",
        }
    }
}

/// Proof that the driver acted: the digest of the approved action, the observation it was spent
/// on, and `Delivery::DeliveredUnverified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionReceipt {
    pub digest: [u8; 32],
    pub observation: ObservationId,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOutcome {
    Executed(ActionReceipt),
    /// Nothing was sent.
    Refused(DriverError),
    Failed {
        error: DriverError,
        delivery: Delivery,
    },
}

impl ExecOutcome {
    /// Fixed token for the audit result: `"ok"` or a [`DriverError::code`].
    pub fn result_code(&self) -> &'static str {
        match self {
            ExecOutcome::Executed(_) => "ok",
            ExecOutcome::Refused(e) | ExecOutcome::Failed { error: e, .. } => e.code(),
        }
    }

    /// The value for `PolicyGate::record_result`.
    pub fn exec_result(&self) -> policy::ExecResult {
        match self {
            ExecOutcome::Executed(_) => policy::ExecResult::executed(),
            ExecOutcome::Refused(e) => policy::ExecResult::refused(e.code()),
            ExecOutcome::Failed { error, .. } => policy::ExecResult::failed(error.code()),
        }
    }

    pub fn is_executed(&self) -> bool {
        matches!(self, ExecOutcome::Executed(_))
    }
}
