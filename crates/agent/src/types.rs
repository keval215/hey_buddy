//! Types and traits of ADR 0003 section 2. Everything here is plain data and fixed tokens:
//! no `Debug` or `Display` output carries a transcript, a prompt, model text or a plan.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

/// The closed list of plan/v1 operations (ADR 0003 section 4). Order is the schema order.
pub const PLAN_OPS: &[&str] = &[
    "observe",
    "find_text",
    "answer_from_screen",
    "scroll",
    "focus_window",
    "open_app",
    "click",
    "type",
    "submit",
    "key_combo",
    "copy_selection",
];

/// Hard caps (ADR 0003 sections 2 to 4). Core enforces the same numbers independently.
pub const MAX_STEPS: u8 = 12;
pub const MAX_MODEL_CALLS: u8 = 3;
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
pub const MAX_PROMPT_BYTES: usize = 6 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// What the planner may know. There is deliberately NO field for screen text, window titles,
/// element labels, observations or tool results: the type cannot carry them (ADR 0003 section 10).
#[derive(Clone)]
pub struct PlanInput {
    pub task_id: TaskId,
    pub transcript: Arc<str>,
    pub context: SystemContext,
    pub limits: PlanLimits,
}

impl fmt::Debug for PlanInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PlanInput(task {}, transcript {} B)",
            self.task_id.0,
            self.transcript.len()
        )
    }
}

#[derive(Clone)]
pub struct SystemContext {
    /// App id only; core passes it only if the text verdict was Clear and it is not deny-listed.
    pub foreground_app: Option<Arc<str>>,
    pub ops: &'static [&'static str],
    pub locale: &'static str,
}

impl SystemContext {
    /// The default context: all ops, English, no foreground app.
    pub const fn new() -> Self {
        Self {
            foreground_app: None,
            ops: PLAN_OPS,
            locale: "en",
        }
    }
}

impl Default for SystemContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SystemContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SystemContext(app {}, {} ops)",
            if self.foreground_app.is_some() {
                "set"
            } else {
                "none"
            },
            self.ops.len()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanLimits {
    pub max_steps: u8,
    pub max_model_calls: u8,
    pub deadline: Duration,
}

impl PlanLimits {
    /// Values above the hard caps are clamped (limits can only tighten).
    pub fn new(max_steps: u8, max_model_calls: u8, deadline: Duration) -> Self {
        Self {
            max_steps: max_steps.clamp(1, MAX_STEPS),
            max_model_calls: max_model_calls.clamp(1, MAX_MODEL_CALLS),
            deadline,
        }
    }
    /// 60 s deadline (local model).
    pub fn local() -> Self {
        Self::new(MAX_STEPS, MAX_MODEL_CALLS, Duration::from_secs(60))
    }
    /// 45 s deadline (cloud model).
    pub fn cloud() -> Self {
        Self::new(MAX_STEPS, MAX_MODEL_CALLS, Duration::from_secs(45))
    }
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self::local()
    }
}

/// Untrusted plan bytes from the engine (<= 16 KiB). `Debug` prints the size only; it is
/// never logged. Core's `policy::validate` is the authority on what it means.
#[derive(Clone, PartialEq, Eq)]
pub struct PlanDraftJson(String);

impl PlanDraftJson {
    /// `Err(())` when the draft is larger than 16 KiB.
    #[allow(clippy::result_unit_err)]
    pub fn new(json: String) -> Result<Self, ()> {
        if json.len() > MAX_DRAFT_BYTES {
            Err(())
        } else {
            Ok(Self(json))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for PlanDraftJson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlanDraftJson({} B)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOutcome {
    Draft(PlanDraftJson),
    /// A fixed token such as `cannot_plan` or `invalid_plan`.
    CannotPlan(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPurpose {
    Plan,
    Repair,
}

/// The only schema an engine can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSchema {
    PlanV1,
}

impl OutputSchema {
    pub fn name(self) -> &'static str {
        match self {
            OutputSchema::PlanV1 => "plan_v1",
        }
    }
    /// The closed JSON schema.
    pub fn json(self) -> &'static serde_json::Value {
        match self {
            OutputSchema::PlanV1 => crate::schema::plan_v1_schema(),
        }
    }
}

/// One model request. `system` and `user` are each <= 6 KiB (core re-checks).
#[derive(Clone)]
pub struct ModelCall {
    pub task_id: TaskId,
    pub purpose: CallPurpose,
    pub system: String,
    pub user: String,
    pub schema: OutputSchema,
}

impl fmt::Debug for ModelCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ModelCall(task {}, {:?}, system {} B, user {} B)",
            self.task_id.0,
            self.purpose,
            self.system.len(),
            self.user.len()
        )
    }
}

/// Why core refused or failed a model call. Every payload is a fixed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    #[error("budget_exceeded")]
    BudgetExceeded,
    #[error("too_large")]
    TooLarge,
    #[error("local_only")]
    LocalOnly,
    #[error("consent_denied")]
    ConsentDenied,
    #[error("model_error")]
    Model(&'static str),
    #[error("killed")]
    Killed,
}

impl GatewayError {
    /// Wire code (fixed vocabulary, see [`GatewayError::from_code`]).
    pub fn code(self) -> &'static str {
        match self {
            GatewayError::BudgetExceeded => "budget_exceeded",
            GatewayError::TooLarge => "too_large",
            GatewayError::LocalOnly => "local_only",
            GatewayError::ConsentDenied => "consent_denied",
            GatewayError::Model(_) => "model_error",
            GatewayError::Killed => "killed",
        }
    }
    /// Unknown codes become `Model("unknown_code")`.
    pub fn from_code(code: &str) -> Self {
        match code {
            "budget_exceeded" => GatewayError::BudgetExceeded,
            "too_large" => GatewayError::TooLarge,
            "local_only" => GatewayError::LocalOnly,
            "consent_denied" => GatewayError::ConsentDenied,
            "killed" => GatewayError::Killed,
            "model_error" => GatewayError::Model("model_error"),
            _ => GatewayError::Model("unknown_code"),
        }
    }
}

#[async_trait]
pub trait ModelGateway: Send + Sync {
    /// The ONLY way an engine reaches a model. Core enforces budget, sizes, locality and consent.
    async fn complete(&self, call: ModelCall) -> Result<String, GatewayError>;
}

/// Fixed tokens only; `Display` never carries content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// `sandbox_failed`, `spawn_failed`, `unsupported_platform`.
    #[error("unavailable")]
    Unavailable(&'static str),
    #[error("protocol")]
    Protocol(&'static str),
    #[error("timeout")]
    Timeout,
    #[error("gateway")]
    Gateway(GatewayError),
    #[error("limit_exceeded")]
    LimitExceeded(&'static str),
    #[error("killed")]
    Killed,
    #[error("cancelled")]
    Cancelled,
}

#[async_trait]
pub trait AgentEngine: Send + Sync {
    fn id(&self) -> &str;
    /// One plan per call; dropping the future cancels.
    async fn plan(
        &self,
        input: PlanInput,
        models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError>;
    /// Non-blocking, never waits on the engine; after `kill` this instance is dead.
    fn kill(&self);
}
