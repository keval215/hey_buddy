//! Wire protocol (ADR 0003 section 3): stdio JSON lines between core and the engine process.
//!
//! * One JSON object per line, UTF-8, no NUL, at most [`MAX_LINE`] bytes; empty lines are errors.
//! * `deny_unknown_fields` everywhere; versioned by the `Hello`/`Ready` handshake (`"v":1`).
//! * Direction-typed: [`CoreToEngine`] and [`EngineToCore`] are different types, so the engine has
//!   NO message that approves, executes or observes, and a forged `{"t":"approve"}` or
//!   `{"t":"execute"}` line is just an unknown variant (a protocol violation: the caller kills the
//!   engine).
//! * The [`Codec`] is pure (no I/O); [`LineSplitter`] and [`LineReader`] turn bytes into lines with
//!   the cap enforced before a line is complete; [`EngineMessageTracker`] enforces the per-task
//!   message budget, unique call ids and task identity.
//!
//! Errors are fixed tokens: nothing from the offending line is ever copied into an error.

use std::collections::VecDeque;
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::types::{
    CallPurpose, GatewayError, ModelCall, OutputSchema, PlanInput, PlanLimits, SystemContext,
    TaskId, MAX_DRAFT_BYTES, MAX_PROMPT_BYTES, MAX_RESPONSE_BYTES, MAX_STEPS, PLAN_OPS,
};

pub const PROTOCOL_VERSION: u32 = 1;
/// Longest line (excluding the newline).
pub const MAX_LINE: usize = 64 * 1024;
/// Engine-to-core messages accepted per task (the process is one task).
pub const MAX_MESSAGES_PER_TASK: usize = 64;
/// Largest transcript on the wire (core's `policy` accepts the same).
pub const MAX_TRANSCRIPT_WIRE: usize = 16 * 1024;
const MAX_APP_ID: usize = 128;
const MAX_CODE: usize = 32;
const MAX_ENGINE_ID: usize = 64;

/// Why a line or message was refused. A violation always ends with the caller KILLING the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("line_too_long")]
    LineTooLong,
    #[error("empty_line")]
    EmptyLine,
    #[error("not_utf8")]
    NotUtf8,
    #[error("nul_byte")]
    Nul,
    #[error("malformed")]
    Malformed,
    #[error("bad_version")]
    BadVersion,
    #[error("field_too_large")]
    FieldTooLarge,
    #[error("bad_field")]
    BadField,
    #[error("too_many_messages")]
    TooManyMessages,
    #[error("duplicate_call_id")]
    DuplicateCallId,
    #[error("wrong_task")]
    WrongTask,
    #[error("unexpected_message")]
    Unexpected,
    #[error("truncated")]
    Truncated,
    #[error("io")]
    Io,
}

impl ProtocolError {
    /// Fixed token for `EngineError::Protocol`.
    pub fn code(self) -> &'static str {
        match self {
            ProtocolError::LineTooLong => "line_too_long",
            ProtocolError::EmptyLine => "empty_line",
            ProtocolError::NotUtf8 => "not_utf8",
            ProtocolError::Nul => "nul_byte",
            ProtocolError::Malformed => "malformed",
            ProtocolError::BadVersion => "bad_version",
            ProtocolError::FieldTooLarge => "field_too_large",
            ProtocolError::BadField => "bad_field",
            ProtocolError::TooManyMessages => "too_many_messages",
            ProtocolError::DuplicateCallId => "duplicate_call_id",
            ProtocolError::WrongTask => "wrong_task",
            ProtocolError::Unexpected => "unexpected_message",
            ProtocolError::Truncated => "truncated",
            ProtocolError::Io => "io",
        }
    }
}

// ----------------------------------------------------------------------------------- messages

/// The `PlanTask` payload (a [`PlanInput`] in wire form).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTaskWire {
    pub task_id: u64,
    pub transcript: String,
    pub foreground_app: Option<String>,
    pub ops: Vec<String>,
    pub locale: String,
    pub max_steps: u8,
    pub max_model_calls: u8,
    pub deadline_ms: u64,
}

impl fmt::Debug for PlanTaskWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PlanTaskWire(task {}, transcript {} B)",
            self.task_id,
            self.transcript.len()
        )
    }
}

impl PlanTaskWire {
    pub fn from_input(i: &PlanInput) -> Self {
        Self {
            task_id: i.task_id.0,
            transcript: i.transcript.to_string(),
            foreground_app: i.context.foreground_app.as_ref().map(|a| a.to_string()),
            ops: i.context.ops.iter().map(|o| (*o).to_string()).collect(),
            locale: i.context.locale.to_string(),
            max_steps: i.limits.max_steps,
            max_model_calls: i.limits.max_model_calls,
            deadline_ms: u64::try_from(i.limits.deadline.as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Engine side: validate and map to a [`PlanInput`]. Ops must be a subset of the closed list
    /// (mapped to the static names); the locale must be `en`; limits are clamped to the hard caps.
    pub fn into_input(self) -> Result<PlanInput, ProtocolError> {
        self.check()?;
        let mut ops: Vec<&'static str> = Vec::with_capacity(self.ops.len());
        for o in &self.ops {
            let s = PLAN_OPS
                .iter()
                .find(|k| **k == o.as_str())
                .ok_or(ProtocolError::BadField)?;
            ops.push(s);
        }
        let ops = static_ops(&ops).ok_or(ProtocolError::BadField)?;
        Ok(PlanInput {
            task_id: TaskId(self.task_id),
            transcript: Arc::from(self.transcript),
            context: SystemContext {
                foreground_app: self.foreground_app.map(Arc::from),
                ops,
                locale: "en",
            },
            limits: PlanLimits::new(
                self.max_steps,
                self.max_model_calls,
                Duration::from_millis(self.deadline_ms),
            ),
        })
    }

    fn check(&self) -> Result<(), ProtocolError> {
        if self.transcript.len() > MAX_TRANSCRIPT_WIRE {
            return Err(ProtocolError::FieldTooLarge);
        }
        if self
            .foreground_app
            .as_ref()
            .is_some_and(|a| a.len() > MAX_APP_ID)
        {
            return Err(ProtocolError::FieldTooLarge);
        }
        if self.ops.len() > PLAN_OPS.len() || self.locale != "en" {
            return Err(ProtocolError::BadField);
        }
        if self.max_steps == 0
            || self.max_steps > MAX_STEPS
            || self.max_model_calls == 0
            || self.max_model_calls > crate::types::MAX_MODEL_CALLS
        {
            return Err(ProtocolError::BadField);
        }
        Ok(())
    }
}

/// `SystemContext.ops` is a `'static` slice, so only two tables are representable on the wire:
/// the full closed list and the read-only subset. Anything else is refused (never widened).
fn static_ops(requested: &[&'static str]) -> Option<&'static [&'static str]> {
    const READ_ONLY: &[&str] = &["observe", "find_text", "answer_from_screen"];
    if requested == PLAN_OPS {
        Some(PLAN_OPS)
    } else if requested == READ_ONLY {
        Some(READ_ONLY)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelOutcome {
    #[serde(rename = "ok")]
    Text { text: String },
    #[serde(rename = "err")]
    Failed { code: String },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WirePurpose {
    Plan,
    Repair,
}

impl fmt::Debug for WirePurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WirePurpose::Plan => "plan",
            WirePurpose::Repair => "repair",
        })
    }
}

/// Core to engine. There is no message that carries an observation, a tool result or an approval.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreToEngine {
    Hello {
        v: u32,
        session: u64,
    },
    PlanTask(PlanTaskWire),
    ModelResult {
        call_id: u64,
        result: ModelOutcome,
    },
    Cancel {
        task_id: u64,
    },
    /// Struct variant on purpose: serde ignores unknown fields on a UNIT variant of an internally
    /// tagged enum even with `deny_unknown_fields`.
    Shutdown {},
}

/// Engine to core. Only proposals and model requests.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineToCore {
    Ready {
        engine_id: String,
        v: u32,
    },
    ModelCall {
        call_id: u64,
        task_id: u64,
        purpose: WirePurpose,
        system: String,
        user: String,
        schema: String,
    },
    PlanDraft {
        task_id: u64,
        json: String,
    },
    CannotPlan {
        task_id: u64,
        code: String,
    },
}

// Content-free Debug (the transcript, prompts, model text and plans never print).
impl fmt::Debug for CoreToEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreToEngine::Hello { v, .. } => write!(f, "Hello(v{v})"),
            CoreToEngine::PlanTask(p) => write!(f, "{p:?}"),
            CoreToEngine::ModelResult { call_id, result } => write!(
                f,
                "ModelResult(call {call_id}, {})",
                match result {
                    ModelOutcome::Text { text } => format!("ok {} B", text.len()),
                    ModelOutcome::Failed { .. } => "err".to_string(),
                }
            ),
            CoreToEngine::Cancel { task_id } => write!(f, "Cancel(task {task_id})"),
            CoreToEngine::Shutdown {} => f.write_str("Shutdown"),
        }
    }
}

impl fmt::Debug for EngineToCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineToCore::Ready { v, .. } => write!(f, "Ready(v{v})"),
            EngineToCore::ModelCall {
                call_id,
                task_id,
                purpose,
                system,
                user,
                ..
            } => write!(
                f,
                "ModelCall(call {call_id}, task {task_id}, {purpose:?}, system {} B, user {} B)",
                system.len(),
                user.len()
            ),
            EngineToCore::PlanDraft { task_id, json } => {
                write!(f, "PlanDraft(task {task_id}, {} B)", json.len())
            }
            EngineToCore::CannotPlan { task_id, .. } => write!(f, "CannotPlan(task {task_id})"),
        }
    }
}

impl EngineToCore {
    /// Wire form of a [`ModelCall`] request.
    pub fn model_call(call_id: u64, c: &ModelCall) -> Self {
        EngineToCore::ModelCall {
            call_id,
            task_id: c.task_id.0,
            purpose: match c.purpose {
                CallPurpose::Plan => WirePurpose::Plan,
                CallPurpose::Repair => WirePurpose::Repair,
            },
            system: c.system.clone(),
            user: c.user.clone(),
            schema: c.schema.name().to_string(),
        }
    }
}

impl ModelOutcome {
    pub fn from_result(r: &Result<String, GatewayError>) -> Self {
        match r {
            Ok(t) => ModelOutcome::Text { text: t.clone() },
            Err(e) => ModelOutcome::Failed {
                code: e.code().to_string(),
            },
        }
    }
    pub fn into_result(self) -> Result<String, GatewayError> {
        match self {
            ModelOutcome::Text { text } => Ok(text),
            ModelOutcome::Failed { code } => Err(GatewayError::from_code(&code)),
        }
    }
}

/// The wire schema name to [`OutputSchema`] (`plan_v1` only).
pub fn schema_from_name(name: &str) -> Option<OutputSchema> {
    (name == OutputSchema::PlanV1.name()).then_some(OutputSchema::PlanV1)
}

impl WirePurpose {
    pub fn into_purpose(self) -> CallPurpose {
        match self {
            WirePurpose::Plan => CallPurpose::Plan,
            WirePurpose::Repair => CallPurpose::Repair,
        }
    }
}

// ------------------------------------------------------------------------------------- codec

fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn engine_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ENGINE_ID
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

/// Pure encode/decode with every cap. No I/O.
pub struct Codec;

impl Codec {
    /// Line bytes (without the newline) to text: size, UTF-8, NUL, no embedded newline.
    fn text(line: &[u8]) -> Result<&str, ProtocolError> {
        if line.len() > MAX_LINE {
            return Err(ProtocolError::LineTooLong);
        }
        if line.is_empty() {
            return Err(ProtocolError::EmptyLine);
        }
        let s = std::str::from_utf8(line).map_err(|_| ProtocolError::NotUtf8)?;
        if line.contains(&0) {
            return Err(ProtocolError::Nul);
        }
        if line.contains(&b'\n') {
            return Err(ProtocolError::Malformed);
        }
        Ok(s)
    }

    pub fn decode_core(line: &[u8]) -> Result<CoreToEngine, ProtocolError> {
        let s = Self::text(line)?;
        let m: CoreToEngine = serde_json::from_str(s).map_err(|_| ProtocolError::Malformed)?;
        Self::validate_core(&m)?;
        Ok(m)
    }

    pub fn decode_engine(line: &[u8]) -> Result<EngineToCore, ProtocolError> {
        let s = Self::text(line)?;
        let m: EngineToCore = serde_json::from_str(s).map_err(|_| ProtocolError::Malformed)?;
        Self::validate_engine(&m)?;
        Ok(m)
    }

    /// The line INCLUDING the trailing newline.
    pub fn encode_core(m: &CoreToEngine) -> Result<Vec<u8>, ProtocolError> {
        Self::validate_core(m)?;
        Self::finish(serde_json::to_vec(m).map_err(|_| ProtocolError::Malformed)?)
    }

    pub fn encode_engine(m: &EngineToCore) -> Result<Vec<u8>, ProtocolError> {
        Self::validate_engine(m)?;
        Self::finish(serde_json::to_vec(m).map_err(|_| ProtocolError::Malformed)?)
    }

    fn finish(mut v: Vec<u8>) -> Result<Vec<u8>, ProtocolError> {
        if v.len() > MAX_LINE {
            return Err(ProtocolError::LineTooLong);
        }
        v.push(b'\n');
        Ok(v)
    }

    pub fn validate_core(m: &CoreToEngine) -> Result<(), ProtocolError> {
        match m {
            CoreToEngine::Hello { v, .. } => {
                if *v != PROTOCOL_VERSION {
                    return Err(ProtocolError::BadVersion);
                }
            }
            CoreToEngine::PlanTask(p) => p.check()?,
            CoreToEngine::ModelResult { result, .. } => match result {
                ModelOutcome::Text { text } => {
                    if text.len() > MAX_RESPONSE_BYTES {
                        return Err(ProtocolError::FieldTooLarge);
                    }
                }
                ModelOutcome::Failed { code } => {
                    if !token_ok(code, MAX_CODE) {
                        return Err(ProtocolError::BadField);
                    }
                }
            },
            CoreToEngine::Cancel { .. } | CoreToEngine::Shutdown {} => {}
        }
        Ok(())
    }

    pub fn validate_engine(m: &EngineToCore) -> Result<(), ProtocolError> {
        match m {
            EngineToCore::Ready { engine_id, v } => {
                if *v != PROTOCOL_VERSION {
                    return Err(ProtocolError::BadVersion);
                }
                if !engine_id_ok(engine_id) {
                    return Err(ProtocolError::BadField);
                }
            }
            EngineToCore::ModelCall {
                system,
                user,
                schema,
                ..
            } => {
                if system.len() > MAX_PROMPT_BYTES || user.len() > MAX_PROMPT_BYTES {
                    return Err(ProtocolError::FieldTooLarge);
                }
                if schema_from_name(schema).is_none() {
                    return Err(ProtocolError::BadField);
                }
            }
            EngineToCore::PlanDraft { json, .. } => {
                if json.len() > MAX_DRAFT_BYTES {
                    return Err(ProtocolError::FieldTooLarge);
                }
            }
            EngineToCore::CannotPlan { code, .. } => {
                if !token_ok(code, MAX_CODE) {
                    return Err(ProtocolError::BadField);
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------------ line splitting

/// Splits a byte stream into lines, enforcing [`MAX_LINE`] BEFORE a line is complete (so an
/// endless line never grows the buffer past the cap).
#[derive(Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the complete lines (without newline). Error on an over-long line.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ProtocolError> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                out.push(std::mem::take(&mut self.buf));
            } else {
                if self.buf.len() >= MAX_LINE {
                    self.buf.clear();
                    return Err(ProtocolError::LineTooLong);
                }
                self.buf.push(b);
            }
        }
        Ok(out)
    }

    /// Bytes of an incomplete line (a non-zero value at EOF is a truncated line).
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Blocking line reader over any `Read` (a child's stdout, stdin). Strict: see [`LineSplitter`].
pub struct LineReader<R: Read> {
    inner: R,
    splitter: LineSplitter,
    ready: VecDeque<Vec<u8>>,
    eof: bool,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            splitter: LineSplitter::new(),
            ready: VecDeque::new(),
            eof: false,
        }
    }

    /// `Ok(None)` is a clean EOF (no partial line); a partial last line is `Truncated`.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>, ProtocolError> {
        loop {
            if let Some(l) = self.ready.pop_front() {
                return Ok(Some(l));
            }
            if self.eof {
                return Ok(None);
            }
            let mut chunk = [0u8; 4096];
            match self.inner.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    if self.splitter.pending() > 0 {
                        return Err(ProtocolError::Truncated);
                    }
                }
                Ok(n) => self.ready.extend(self.splitter.push(&chunk[..n])?),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ProtocolError::Io),
            }
        }
    }
}

// ------------------------------------------------------------------------------------ tracker

/// Core's per-task view of what the engine sent: message budget, handshake order, task identity,
/// unique call ids and "nothing after the final message". Pure.
pub struct EngineMessageTracker {
    task: u64,
    count: usize,
    ready: bool,
    done: bool,
    seen: Vec<u64>,
}

impl EngineMessageTracker {
    pub fn new(task: TaskId) -> Self {
        Self {
            task: task.0,
            count: 0,
            ready: false,
            done: false,
            seen: Vec::new(),
        }
    }

    pub fn accept(&mut self, m: &EngineToCore) -> Result<(), ProtocolError> {
        self.count += 1;
        if self.count > MAX_MESSAGES_PER_TASK {
            return Err(ProtocolError::TooManyMessages);
        }
        if self.done {
            return Err(ProtocolError::Unexpected);
        }
        match m {
            EngineToCore::Ready { .. } => {
                if self.ready {
                    return Err(ProtocolError::Unexpected);
                }
                self.ready = true;
            }
            _ if !self.ready => return Err(ProtocolError::Unexpected),
            EngineToCore::ModelCall {
                call_id, task_id, ..
            } => {
                if *task_id != self.task {
                    return Err(ProtocolError::WrongTask);
                }
                if self.seen.contains(call_id) {
                    return Err(ProtocolError::DuplicateCallId);
                }
                self.seen.push(*call_id);
            }
            EngineToCore::PlanDraft { task_id, .. } | EngineToCore::CannotPlan { task_id, .. } => {
                if *task_id != self.task {
                    return Err(ProtocolError::WrongTask);
                }
                self.done = true;
            }
        }
        Ok(())
    }

    pub fn messages_seen(&self) -> usize {
        self.count
    }
}
