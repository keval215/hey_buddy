//! Plain-data events the gate emits (ADR 0003 section 11). The sink is called AFTER the state
//! change is committed and NEVER while the gate holds a lock (L-013), so a sink may call back into
//! the gate. Events carry ids, digests and fixed tokens only: never labels, text or plan JSON.

use std::sync::Mutex;

use crate::config::TaskOrigin;
use crate::ids::{ApprovalId, TaskId};
use crate::tier::Tier;

/// What an approval is for (the Submit variant has its own panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    Plan,
    Step,
    Submit,
}

impl ApprovalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalKind::Plan => "plan",
            ApprovalKind::Step => "step",
            ApprovalKind::Submit => "submit",
        }
    }
}

/// Why an approval was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyWhy {
    User,
    Timeout,
    UiUnavailable,
    Voided,
    /// The waiter was dropped.
    Cancelled,
}

impl DenyWhy {
    pub fn as_str(self) -> &'static str {
        match self {
            DenyWhy::User => "user",
            DenyWhy::Timeout => "timeout",
            DenyWhy::UiUnavailable => "ui_unavailable",
            DenyWhy::Voided => "voided",
            DenyWhy::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEventKind {
    TaskStarted {
        origin: TaskOrigin,
    },
    /// `digest8` is the plan digest without dry-run bindings (the final digest is in
    /// `ApprovalRequested`).
    PlanProposed {
        digest8: String,
        steps: u16,
    },
    ActionSimulated {
        step: u16,
        /// `None` when the step could not be bound against the current observation.
        tier: Option<Tier>,
    },
    ApprovalRequested {
        approval_id: ApprovalId,
        /// Full lowercase hex of the plan or action digest: the approval window echoes it back
        /// in `decide`, the UI shows only the first 8 digits.
        digest: String,
        kind: ApprovalKind,
        timeout_s: u64,
        /// The step changed since the plan was approved (drift) or was never shown bound.
        changed: bool,
    },
    ApprovalGranted {
        approval_id: ApprovalId,
        kind: ApprovalKind,
    },
    ApprovalDenied {
        approval_id: ApprovalId,
        why: DenyWhy,
    },
    ActionExecuted {
        step: u16,
        digest8: String,
        result: &'static str,
    },
    ActionRefused {
        step: u16,
        reason: &'static str,
    },
    /// An unattended task reached a step it may not run.
    TaskPaused {
        reason: &'static str,
    },
    Killed {
        reason: &'static str,
    },
    TaskFinished {
        outcome: &'static str,
    },
}

/// `seq` is per gate (per app run) and strictly increasing in commit order; sinks may see events
/// from different threads out of order and should order by `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvent {
    pub seq: u64,
    pub task: Option<TaskId>,
    pub kind: AgentEventKind,
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: &AgentEvent);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullEvents;

impl EventSink for NullEvents {
    fn emit(&self, _event: &AgentEvent) {}
}

/// Collects events (tests).
#[derive(Debug, Default)]
pub struct MemoryEvents {
    events: Mutex<Vec<AgentEvent>>,
}

impl MemoryEvents {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn events(&self) -> Vec<AgentEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn kinds(&self) -> Vec<AgentEventKind> {
        self.events().into_iter().map(|e| e.kind).collect()
    }
}

impl EventSink for MemoryEvents {
    fn emit(&self, event: &AgentEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
    }
}
