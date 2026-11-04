//! Gate configuration, mode and the fixed timing constants of ADR 0003 sections 6-7.

use std::time::Duration;

use crate::never_apps::NeverApps;

/// Plan approval timeout. Timeout is Deny and ends the task.
pub const PLAN_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Single step approval timeout. Timeout is Deny and ends the task.
pub const STEP_APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// Decisions arriving sooner than this after an approval was shown are ignored.
pub const INPUT_GUARD: Duration = Duration::from_millis(500);
/// A plan grant dies this long after approval.
pub const PLAN_GRANT_TTL: Duration = Duration::from_secs(600);
/// A plan grant dies after this long without step progress.
pub const STALL_TTL: Duration = Duration::from_secs(30);
/// Maximum age of the observation (and of the bound step) an action is minted from. Must be at
/// least the step approval timeout (ADR 0004 section 6).
pub const OBSERVATION_MAX_AGE: Duration = Duration::from_secs(60);
/// `ApprovedAction::check_live` fails after this long.
pub const ACTION_TTL: Duration = Duration::from_secs(2);
/// Minimum time between two mints.
pub const MIN_MINT_GAP: Duration = Duration::from_millis(250);
/// Maximum mints in any rolling minute.
pub const MAX_MINTS_PER_MINUTE: usize = 30;

/// Observe mode (default) never mints; Live is per task and bound to the plan digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Observe,
    Live,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Observe => "observe",
            Mode::Live => "live",
        }
    }
}

/// Who started the task (D-006: unattended runs are Read+Reversible only; scheduled tasks are
/// Read-only in v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskOrigin {
    /// The user asked, in person.
    #[default]
    Interactive,
    /// A timer or trigger started it: Read tier only.
    Scheduled,
    /// The user started it and walked away: Read and Reversible only.
    Background,
}

impl TaskOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskOrigin::Interactive => "interactive",
            TaskOrigin::Scheduled => "scheduled",
            TaskOrigin::Background => "background",
        }
    }

    /// Larger = stricter.
    fn strictness(self) -> u8 {
        match self {
            TaskOrigin::Interactive => 0,
            TaskOrigin::Background => 1,
            TaskOrigin::Scheduled => 2,
        }
    }

    /// STRICTER WINS.
    pub fn stricter(self, other: TaskOrigin) -> TaskOrigin {
        if other.strictness() > self.strictness() {
            other
        } else {
            self
        }
    }
}

/// Everything configurable. Timeouts are NOT configurable: they are part of the permission model.
#[derive(Debug, Clone, Default)]
pub struct PolicyConfig {
    /// Default origin of tasks proposed with [`crate::PolicyGate::propose`].
    pub origin: TaskOrigin,
    /// The Never-app list the gate re-classifies with (the built-in list is always included).
    pub never_apps: NeverApps,
}
