//! The agent engine's only way to a model (ADR 0003 sections 2 and 10): [`CoreModelGateway`], plus
//! the [`ProcessEngineKiller`] that plugs the engine into the kill switch.
//!
//! The gateway never trusts the engine (which may be a compromised process): it enforces, per
//! task, at most [`MAX_CALLS`] model calls (every refused or failed attempt counts), system and
//! user text of at most 6 KiB each, a response of at most 16 KiB, and the settings' `local_only`
//! rule. The request is TEXT ONLY (`ScreenContext::None`: no screen text, no image, ever) with the
//! plan/v1 output schema, built into an `llm::LlmRequest` and run through
//! `Buddy::prepare_and_consent`, so a cloud call gets the exact-bytes preview ("Agent planning")
//! and needs a Send for EACH call, repairs included; local calls need no preview. Every error is
//! a fixed token ([`agent::GatewayError`]): nothing from the model or the request is copied.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use agent::{
    AgentEngine, GatewayError, ModelCall, ModelGateway, OutputSchema as AgentSchema, TaskId,
};
use async_trait::async_trait;
use futures::StreamExt;
use llm::{LlmDelta, LlmRequest, OutputSchema, ScreenContext, StopReason};

use crate::api::ProviderChoice;
use crate::buddy::Buddy;
use crate::error::CoreError;
use crate::kill::EngineKiller;
use crate::turn::ProviderSource;

/// Model calls per task (the first plan plus at most two repairs).
pub const MAX_CALLS: u8 = 3;
/// `system` and `user` of one call, each.
pub const MAX_PROMPT_BYTES: usize = 6 * 1024;
/// One model response.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The `llm` output schema for an engine schema.
fn llm_schema(s: AgentSchema) -> OutputSchema {
    OutputSchema::new(s.name(), s.json().clone())
}

/// One gateway per task: it owns that task's call budget.
pub struct CoreModelGateway {
    buddy: Arc<Buddy>,
    providers: Arc<dyn ProviderSource>,
    task: TaskId,
    calls: AtomicU8,
    killed: AtomicBool,
}

impl CoreModelGateway {
    pub fn new(buddy: Arc<Buddy>, providers: Arc<dyn ProviderSource>, task: TaskId) -> Self {
        Self {
            buddy,
            providers,
            task,
            calls: AtomicU8::new(0),
            killed: AtomicBool::new(false),
        }
    }

    /// After this every call is `Killed`.
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
    }

    /// `kill` was called.
    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    /// Calls attempted so far (refused attempts count).
    pub fn calls_used(&self) -> u8 {
        self.calls.load(Ordering::SeqCst).min(MAX_CALLS)
    }

    fn provider(&self) -> Result<Arc<dyn llm::LlmProvider>, GatewayError> {
        let s = self.buddy.get_settings();
        match s.provider {
            ProviderChoice::Ollama => self
                .providers
                .local(&s.profile)
                .map_err(|_| GatewayError::Model("provider_unavailable")),
            ProviderChoice::Anthropic => {
                if s.local_only {
                    return Err(GatewayError::LocalOnly);
                }
                if !s.cloud_configured() {
                    return Err(GatewayError::Model("cloud_not_configured"));
                }
                if !s.has_api_key {
                    return Err(GatewayError::Model("no_credentials"));
                }
                self.providers
                    .cloud(s.anthropic_model.trim())
                    .map_err(|_| GatewayError::Model("provider_unavailable"))
            }
        }
    }
}

#[async_trait]
impl ModelGateway for CoreModelGateway {
    async fn complete(&self, call: ModelCall) -> Result<String, GatewayError> {
        if self.killed.load(Ordering::SeqCst) {
            return Err(GatewayError::Killed);
        }
        // The budget first: an attempt is spent even if a later check refuses it.
        loop {
            let n = self.calls.load(Ordering::SeqCst);
            if n >= MAX_CALLS {
                return Err(GatewayError::BudgetExceeded);
            }
            if self
                .calls
                .compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
        if call.task_id != self.task {
            return Err(GatewayError::Model("wrong_task"));
        }
        if call.system.len() > MAX_PROMPT_BYTES || call.user.len() > MAX_PROMPT_BYTES {
            return Err(GatewayError::TooLarge);
        }
        let provider = self.provider()?;
        // Text only: `ScreenContext::None` is the only constructor used here.
        let req = LlmRequest::new(call.system, call.user, ScreenContext::None)
            .map_err(|_| GatewayError::Model("request_failed"))?
            .with_output_schema(llm_schema(call.schema));
        let (prepared, opt_in) = match self
            .buddy
            .prepare_and_consent(provider.as_ref(), &req)
            .await
        {
            Ok(x) => x,
            Err(CoreError::LocalOnly) => return Err(GatewayError::LocalOnly),
            Err(CoreError::Cancelled) => return Err(GatewayError::ConsentDenied),
            Err(CoreError::Llm(_)) => return Err(GatewayError::Model("prepare_failed")),
        };
        if self.killed.load(Ordering::SeqCst) {
            return Err(GatewayError::Killed);
        }
        let mut deltas = provider
            .stream(prepared, opt_in)
            .await
            .map_err(|_| GatewayError::Model("stream_failed"))?;
        // The WHOLE reply, never a partial one.
        let mut out = String::new();
        let mut complete = false;
        while let Some(item) = deltas.next().await {
            if self.killed.load(Ordering::SeqCst) {
                return Err(GatewayError::Killed);
            }
            match item {
                Ok(LlmDelta::Text(t)) => {
                    if out.len() + t.len() > MAX_RESPONSE_BYTES {
                        return Err(GatewayError::TooLarge);
                    }
                    out.push_str(&t);
                }
                Ok(LlmDelta::Done { stop }) => {
                    if stop == StopReason::MaxTokens {
                        return Err(GatewayError::Model("truncated"));
                    }
                    complete = true;
                    break;
                }
                Err(_) => return Err(GatewayError::Model("stream_failed")),
            }
        }
        if complete {
            Ok(out)
        } else {
            Err(GatewayError::Model("truncated"))
        }
    }
}

/// Plugs the current engine into the kill switch (`KillController::set_engine_killer`).
/// `kill_engine` is non-blocking: it calls `AgentEngine::kill` on whatever engine is attached
/// (for the real one: `TerminateJobObject`). The supervisor (S2-23) attaches the engine it
/// created for the task and detaches it at the end.
#[derive(Default)]
pub struct ProcessEngineKiller {
    current: Mutex<Option<Arc<dyn AgentEngine>>>,
}

impl ProcessEngineKiller {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn attach(&self, engine: Arc<dyn AgentEngine>) {
        *lock(&self.current) = Some(engine);
    }
    pub fn detach(&self) {
        *lock(&self.current) = None;
    }
    /// Kills the attached engine now (the supervisor's own kill check; the controller's trip
    /// calls the same thing through `EngineKiller`). No-op when none is attached.
    pub fn kill_engine_now(&self) {
        self.kill_engine();
    }
}

impl EngineKiller for ProcessEngineKiller {
    fn kill_engine(&self) {
        // Clone out so the (non-blocking) kill never runs under our lock.
        let e = lock(&self.current).clone();
        if let Some(e) = e {
            e.kill();
        }
    }
}
