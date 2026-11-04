//! hey_buddy desktop driver (ADR 0004 sections 6-10, 12, 13; docs/driver.md).
//!
//! The ONLY crate that performs actions. It accepts only a sealed `policy::ApprovedAction` and
//! acts only through a `firewall::LiveTarget` the firewall re-validated a moment earlier. The
//! driver never reads screen content: it holds the latest `firewall::Observation`, exposes only
//! the firewall-redacted [`ObservedScreen`], and its [`Actuator`] has no read methods.
//!
//! Errors and `Debug` output are fixed tokens (L-010): no OS strings, labels or typed text.
//! Typed text appears only inside the `Actuator::send_chars` call.
//!
//! # Compile-time guarantees
//! Stable rustdoc does NOT enforce the `EXXXX` in `compile_fail,EXXXX`; the codes were verified
//! with `rustc` and are re-checked by `tools/check-doctest-codes.sh`.
//!
//! ```compile_fail,E0308
//! // The `Action` vocabulary type is not authority: only an `ApprovedAction` executes.
//! fn needs_approved(_: policy::ApprovedAction) {}
//! fn f(a: action::Action) {
//!     needs_approved(a);
//! }
//! ```
//! ```compile_fail,E0599
//! // The sealed approval has no `Clone`: it cannot be replayed.
//! fn f(a: policy::ApprovedAction) {
//!     let _ = a.clone();
//! }
//! ```

mod actuator;
mod driver;
mod error;
mod limits;
mod platform;
mod screen;

#[cfg(feature = "fakes")]
pub mod fake;

pub use actuator::{Actuator, ClickMethod, UnsupportedActuator, MAX_CHUNK_CHARS};
pub use driver::{DesktopDriver, Driver, ObserveOutcome};
pub use error::{ActError, ActionReceipt, Delivery, DriverError, ExecOutcome};
pub use limits::{
    LaunchAllowlist, CALL_DEADLINE, MAX_EXECS_PER_MINUTE, MIN_EXEC_GAP, OBSERVATION_MAX_AGE,
};
pub use screen::ObservedScreen;

#[cfg(test)]
mod tests;
