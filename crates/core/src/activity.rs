//! "Is the agent doing anything right now?" (S2-13). The audit-log clear and the loosening
//! deny-list edits are refused while a task is active or an intent / action is outstanding
//! (refusal code `busy`), so the log of what is happening cannot be wiped mid-run and the
//! deny list cannot be relaxed under a running plan.

use std::sync::Arc;

use policy::{Activity, PolicyGate};

/// Read-only view of the agent's activity. `Buddy` holds `None` until the agent is wired in
/// (`None` means idle: there is no agent to be busy).
pub trait ActivityProbe: Send + Sync {
    fn activity(&self) -> Activity;
}

/// The real probe: the policy gate.
pub struct GateProbe(pub Arc<PolicyGate>);

impl ActivityProbe for GateProbe {
    fn activity(&self) -> Activity {
        self.0.activity()
    }
}

/// A probe with a fixed answer (tests, and a stand-in before the agent exists).
pub struct FixedProbe(pub std::sync::Mutex<Activity>);

impl FixedProbe {
    pub fn idle() -> Self {
        Self(std::sync::Mutex::new(Activity {
            task_live: false,
            busy_state: policy::BusyState::Idle,
        }))
    }
    pub fn set(&self, a: Activity) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = a;
    }
}

impl ActivityProbe for FixedProbe {
    fn activity(&self) -> Activity {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
