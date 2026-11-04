//! The supervisor's view of the desktop driver and of what the gate cannot derive by itself.
//!
//! * [`ObservationSource`]: firewall `observe` through the driver (the driver caches the one
//!   observation an `execute` may spend) and a read-only peek at it for `policy::bind_step`.
//! * [`AgentDriver`]: the source plus `execute(ApprovedAction)` and the emergency release. Blanket
//!   implemented for `driver::Driver<A>`, so tests use the REAL driver over a `FakeActuator`.
//! * [`BindFacts`]: the facts `BindContext` needs that an `Observation` cannot tell
//!   (`higher_integrity`, `focused_secure`, `target_denied`, the focused element). UNKNOWN means
//!   Never at the gate (`missing_fact`), so the safe default is [`UnknownFacts`].

use std::sync::Arc;

use driver::{Actuator, DesktopDriver, Driver, ExecOutcome, ObserveOutcome};
use firewall::{ElementRef, Observation};
use frame::AppDenyPolicy;
use policy::{ApprovedAction, KillSwitch, PlanStep};

use crate::kill::DriverReleaser;

/// Firewall observation through the driver.
pub trait ObservationSource: Send + Sync {
    /// Preflight + `Firewall::observe`; caches the observation (one execute can spend it).
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome;
    /// Runs `f` on the cached, unspent observation. `false` (and `f` not run) when there is none.
    fn with_latest(&self, f: &mut dyn FnMut(&Observation)) -> bool;
}

/// What the supervisor needs from the driver. `execute` consumes the sealed approval.
pub trait AgentDriver: ObservationSource {
    fn execute(
        &self,
        action: ApprovedAction,
        deny: &dyn AppDenyPolicy,
        kill: &KillSwitch,
    ) -> ExecOutcome;
    /// Idempotent; needs no `ApprovedAction`.
    fn emergency_release(&self);
}

impl<A: Actuator + 'static> ObservationSource for Driver<A> {
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome {
        DesktopDriver::observe(self, deny)
    }
    fn with_latest(&self, f: &mut dyn FnMut(&Observation)) -> bool {
        Driver::with_latest(self, |o| f(o)).is_some()
    }
}

impl<A: Actuator + 'static> AgentDriver for Driver<A> {
    fn execute(
        &self,
        action: ApprovedAction,
        deny: &dyn AppDenyPolicy,
        kill: &KillSwitch,
    ) -> ExecOutcome {
        DesktopDriver::execute(self, action, deny, kill)
    }
    fn emergency_release(&self) {
        DesktopDriver::emergency_release(self);
    }
}

/// `f` on the latest unspent observation, with a value back.
pub fn peek<R>(src: &dyn ObservationSource, f: impl FnOnce(&Observation) -> R) -> Option<R> {
    let mut f = Some(f);
    let mut out = None;
    let ran = src.with_latest(&mut |o| {
        if let Some(f) = f.take() {
            out = Some(f(o));
        }
    });
    if ran {
        out
    } else {
        None
    }
}

/// Plugs the driver into the kill switch (`KillController::set_driver_releaser`).
pub struct DriverRelease(pub Arc<dyn AgentDriver>);

impl DriverReleaser for DriverRelease {
    fn emergency_release(&self) {
        self.0.emergency_release();
    }
}

/// What `BindContext` needs besides the observation. `None` = unknown = Never at the gate.
#[derive(Debug, Clone, Copy, Default)]
pub struct BindExtras {
    pub higher_integrity: Option<bool>,
    pub focused_secure: Option<bool>,
    /// `open_app` / `focus_window`: the target app is NOT denied, restricted, elevated or ours.
    pub target_denied: Option<bool>,
    pub focus_target: Option<ElementRef>,
}

/// Source of [`BindExtras`]. `steps` are the plan steps the context is for (the whole plan for the
/// dry run, one step live).
pub trait BindFacts: Send + Sync {
    fn extras(&self, steps: &[PlanStep], obs: &Observation, deny: &dyn AppDenyPolicy)
        -> BindExtras;
}

/// Every fact unknown: every non-Read step is Never (`missing_fact`). The fail-closed default
/// until the platform can answer these questions at bind time.
pub struct UnknownFacts;

impl BindFacts for UnknownFacts {
    fn extras(&self, _: &[PlanStep], _: &Observation, _: &dyn AppDenyPolicy) -> BindExtras {
        BindExtras::default()
    }
}

/// OPT-IN. The integrity level and the focused field are not readable at bind time, but the
/// firewall re-checks both immediately before any input (`Firewall::revalidate` refuses
/// `ElevatedTarget`; `LiveTarget::verify_focus*` refuses a focused password field, in the driver
/// before every key and chunk). This source therefore answers "not higher integrity" and "not
/// focused in a secure field" for the gate and leaves the enforcement to those checks, which end
/// the action with a refusal and send nothing. `target_denied` IS computed here: the user's deny
/// list, the restricted-app list and the auth-surface list, for every `open_app` / `focus_window`
/// in `steps`. The focused element is unknown (key chords and copy stay unbindable).
/// Using this source is a decision of the wiring code, not a default.
pub struct ReverifiedAtExecute;

impl BindFacts for ReverifiedAtExecute {
    fn extras(
        &self,
        steps: &[PlanStep],
        _obs: &Observation,
        deny: &dyn AppDenyPolicy,
    ) -> BindExtras {
        let any_target_denied = steps.iter().any(|s| match s {
            PlanStep::OpenApp { app, .. } | PlanStep::FocusWindow { app, .. } => {
                let id = app.app.as_str();
                deny.is_denied(id)
                    || firewall::is_restricted_app(id)
                    || firewall::is_auth_surface_app(id)
            }
            _ => false,
        });
        BindExtras {
            higher_integrity: Some(false),
            focused_secure: Some(false),
            target_denied: Some(any_target_denied),
            focus_target: None,
        }
    }
}
