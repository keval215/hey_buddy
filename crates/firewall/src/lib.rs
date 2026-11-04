//! hey_buddy privacy firewall (ADR 0001).
//!
//! `PrivacyFirewall::check` is the only producer of [`RedactedFrame`]; its constructor is
//! `pub(crate)`. Flow: `preflight()` (before capture) consults the [`SessionProbe`] and records
//! the secure-field rects; `check(frame)` re-consults the probe, re-queries the detector, blocks
//! if the rects changed, then blacks out the padded rects.
//!
//! Everything fails closed: detector/probe error, timeout (default 200 ms, worker thread),
//! panic (`catch_unwind`), missing preflight record, changed layout, or an unusable frame all
//! yield `Blocked`. Lock screen, auth prompt and secure input yield `Paused`.
//!
//! # ADR compliance
//! `llm` (T9) must accept only [`RedactedFrame`] and never `frame::Frame` or raw image bytes;
//! T9 adds the matching `compile_fail` doctest there. This crate's own `compile_fail` doctests
//! are on [`RedactedFrame`].
//!
//! # Coordinates
//! [`SecureFieldDetector`] is display-independent: it returns rects in the global space that
//! `capture` uses for `DisplayInfo.origin`/`scale` (macOS points; Windows physical virtual-desktop
//! pixels with scale 1.0). The Windows desktop app MUST declare PerMonitorV2 DPI awareness in its
//! manifest, otherwise the Windows detector fails closed. A zero-area detector rect blocks;
//! rects wholly outside the captured display are ignored (no pixels to hide).
//!
//! # Preflight record
//! `preflight()` stamps its record; `check` blocks (`UnstableLayout`) if the record is older than
//! [`DEFAULT_MAX_RECORD_AGE`] (configurable), or was not taken before `frame.captured_at()`.
//! At most one detector call and one probe call are in flight at a time: while a timed-out call
//! is still running, further calls return `Blocked(DetectionTimeout)` without spawning.
//!
//! # Platform notes (untested on the Linux build machine)
//! macOS: AX walk of visible apps for role or subrole `AXSecureTextField`; unreadable trees (or
//! an app with a window but zero AX nodes) trigger `AXManualAccessibility` /
//! `AXEnhancedUserInterface` and a bounded (~300 ms) poll; still unreadable => `IncompleteTree`.
//! Session probe: `IsSecureEventInputEnabled` and loginwindow/lock detection.
//! Windows: UI Automation `IsPassword`; lock/secure desktop via `OpenInputDesktop`.
//!
//! # Known limitations
//! - Windows has NO incomplete-tree detection: lazily built Chromium/Electron UIA trees can
//!   silently return no password fields. Windows never reports `AuthPrompt` (UAC shows as
//!   `LockScreen`).
//! - macOS never reports `AuthPrompt`. Chromium/Electron AX trees are built asynchronously, so a
//!   requery right after enabling `AXManualAccessibility` may still be empty or partial.
//! - macOS skips a small allow-list of SIP-protected system owners (Window Server, Dock, ...).
//! - Real-world detector latency on both platforms is unverified; the platform firewalls use a
//!   longer detector timeout ([`PrivacyFirewall::with_detector_timeout`]).
//!
//! # Text path (ADR 0002 Addendum A)
//! `Firewall::check_text` reads the foreground window's accessibility tree through a
//! [`ScreenTextSource`] and returns [`TextVerdict`]; the only producer of [`RedactedScreenText`]
//! is the pure `redact_tree`. It shares the preflight record with `check`: one preflight
//! authorises exactly one `check` OR one `check_text`. The default `check_text` and
//! `platform_firewall()` on Linux block with `Blocked(Unsupported)` (voice-only turns); on macOS
//! (AX) and Windows (UIA) `platform_firewall()` installs a tree reader (T13, untested on the
//! Linux build machine) that reads ONLY the focused window of the frontmost app, decides
//! secure-ness before reading anything, never reads secure values or window titles, and is
//! bounded (20 000 nodes, depth 64, 1200 ms). Non-password text-field values and window/page
//! titles are never emitted.
//!
//! Errors, `Debug` output and logs contain no pixels, window titles or field values.

mod blackout;
mod builtin;
mod error;
mod firewall;
mod observe;
mod platform;
mod redacted;
mod redacted_text;
mod scrub;
mod sensitive;
mod target;
mod text;

#[cfg(any(test, feature = "fakes"))]
pub mod fake;
#[cfg(any(test, feature = "fakes"))]
mod scripted;

pub use builtin::{is_auth_surface_app, is_restricted_app};
pub use error::{BlockReason, FirewallError, PauseReason, SessionProbeState, SessionState};
pub use firewall::{
    Firewall, PrivacyFirewall, SecureFieldDetector, SessionProbe, Verdict, DEFAULT_MAX_RECORD_AGE,
    DEFAULT_TARGET_TIMEOUT, DEFAULT_TEXT_TIMEOUT, DEFAULT_TIMEOUT,
};
pub use observe::{
    AppId, ElementRef, LabelFlags, Observation, ObservationFlags, ObservationId, ObserveVerdict,
    TargetDescriptor, MAX_APP_ID_CHARS, MAX_VISIBLE_APP_ID_CHARS,
};
pub use platform::{platform_firewall, UnsupportedDetector, UnsupportedSessionProbe};
pub use redacted::RedactedFrame;
pub use redacted_text::RedactedScreenText;
pub use target::{
    hash_automation_id, hash_label, FocusState, ForegroundFacts, LiveTarget, NodeIdentity,
    ObservationSource, ObservedWindow, ResolveRequest, ResolvedFields, ResolvedTarget,
    TargetRefusal, TargetResolver, TargetSel, UnsupportedObservationSource,
    UnsupportedTargetResolver, VisibleApp, WindowFacts,
};
pub use text::{
    is_hostile_char, NodeRole, RawTextNode, RawWindowTree, ScreenTextSource, TextCoverage,
    TextVerdict, UnsupportedTextSource, WithheldReason, MAX_EMITTED_NODES, MAX_LABEL_CHARS,
    MAX_TEXT_CHARS, MAX_VALUE_CHARS, TRUNCATED_MARKER,
};

#[cfg(test)]
mod observe_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;
