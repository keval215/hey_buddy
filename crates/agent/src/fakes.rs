//! Test doubles (feature `fakes`; dev-dependencies only, L-006): [`FakeEngine`] (scripted),
//! [`InProcessMinimalEngine`] and [`HostileEngine`] (a misbehaving CHILD, written as the
//! `main` of an in-process `TestSandbox`). None of them can reach a gate, a driver or the
//! firewall: this crate has no dependency on them.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::minimal::MinimalEngine;
use crate::protocol::{Codec, CoreToEngine, LineReader, MAX_LINE, PROTOCOL_VERSION};
use crate::sandbox::{InProcessMain, PipeReader, PipeWriter};
use crate::types::{AgentEngine, EngineError, ModelCall, ModelGateway, PlanInput, PlanOutcome};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One scripted `plan` call.
pub struct Scripted {
    /// Model calls to make through the gateway first (their results are ignored unless they fail).
    pub gateway_calls: Vec<ModelCall>,
    /// Wait this long, or forever (`None`, until `kill` or the future is dropped).
    pub delay: Option<Duration>,
    pub result: Result<PlanOutcome, EngineError>,
}

impl Scripted {
    pub fn ok(outcome: PlanOutcome) -> Self {
        Self {
            gateway_calls: Vec::new(),
            delay: Some(Duration::ZERO),
            result: Ok(outcome),
        }
    }
    pub fn err(e: EngineError) -> Self {
        Self {
            gateway_calls: Vec::new(),
            delay: Some(Duration::ZERO),
            result: Err(e),
        }
    }
    pub fn hang() -> Self {
        Self {
            gateway_calls: Vec::new(),
            delay: None,
            result: Err(EngineError::Timeout),
        }
    }
}

/// Scripted engine: pops one [`Scripted`] per `plan` call (an empty script is `Cancelled`).
pub struct FakeEngine {
    id: String,
    script: Mutex<VecDeque<Scripted>>,
    killed: AtomicBool,
    kill_tx: watch::Sender<bool>,
    pub kills: AtomicU32,
    pub plans: AtomicU32,
}

impl FakeEngine {
    pub fn new(script: Vec<Scripted>) -> Self {
        let (kill_tx, _) = watch::channel(false);
        Self {
            id: "fake".into(),
            script: Mutex::new(script.into()),
            killed: AtomicBool::new(false),
            kill_tx,
            kills: AtomicU32::new(0),
            plans: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl AgentEngine for FakeEngine {
    fn id(&self) -> &str {
        &self.id
    }

    async fn plan(
        &self,
        _input: PlanInput,
        models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        if self.killed.load(Ordering::SeqCst) {
            return Err(EngineError::Killed);
        }
        let step = lock(&self.script).pop_front();
        let Some(step) = step else {
            return Err(EngineError::Cancelled);
        };
        let mut rx = self.kill_tx.subscribe();
        for c in step.gateway_calls {
            tokio::select! {
                _ = rx.wait_for(|k| *k) => return Err(EngineError::Killed),
                r = models.complete(c) => { r.map_err(EngineError::Gateway)?; }
            }
        }
        match step.delay {
            Some(d) => tokio::select! {
                _ = rx.wait_for(|k| *k) => return Err(EngineError::Killed),
                _ = tokio::time::sleep(d) => {}
            },
            None => {
                let _ = rx.wait_for(|k| *k).await;
                return Err(EngineError::Killed);
            }
        }
        step.result
    }

    fn kill(&self) {
        self.kills.fetch_add(1, Ordering::SeqCst);
        self.killed.store(true, Ordering::SeqCst);
        self.kill_tx.send_replace(true);
    }
}

/// `MinimalEngine` called directly (no process, no pipes). Tests only.
pub struct InProcessMinimalEngine(MinimalEngine);

impl InProcessMinimalEngine {
    pub fn new() -> Self {
        Self(MinimalEngine::with_id("in_process_minimal"))
    }
}

impl Default for InProcessMinimalEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentEngine for InProcessMinimalEngine {
    fn id(&self) -> &str {
        self.0.id()
    }
    async fn plan(
        &self,
        input: PlanInput,
        models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError> {
        self.0.plan(input, models).await
    }
    fn kill(&self) {
        self.0.kill();
    }
}

/// What a hostile child does after the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hostile {
    /// `{"t":"approve",...}`: no such engine message.
    ForgedApprove,
    ForgedExecute,
    ForgedObserve,
    /// A core-to-engine message sent the wrong way.
    ForgedHello,
    /// A valid `plan_draft` plus an extra `approved` field.
    UnknownField,
    /// A line over 64 KiB WITH a newline.
    OversizeLine,
    /// An endless line (no newline).
    EndlessLine,
    /// 1000 model calls back to back.
    ThousandMessages,
    InvalidUtf8,
    NulByte,
    EmptyLine,
    /// Valid `plan_draft` for task id + 1.
    WrongTaskDraft,
    /// `model_call` for another task id.
    WrongTaskModelCall,
    /// Two model calls with the same call id.
    DuplicateCallId,
    /// Five model calls, one after the other (core's budget is three).
    BeyondBudget,
    /// A 20 KiB draft (in one valid line).
    DraftTooLarge,
    /// `ready` with a wrong protocol version.
    BadVersion,
    /// A draft before `ready`.
    DraftBeforeReady,
    /// A 7 KiB system prompt.
    OversizePrompt,
    /// Never answers; ignores `Cancel` and `Shutdown`; ends only when its pipes close.
    Hang,
}

impl Hostile {
    pub fn all() -> &'static [Hostile] {
        use Hostile::*;
        &[
            ForgedApprove,
            ForgedExecute,
            ForgedObserve,
            ForgedHello,
            UnknownField,
            OversizeLine,
            EndlessLine,
            ThousandMessages,
            InvalidUtf8,
            NulByte,
            EmptyLine,
            WrongTaskDraft,
            WrongTaskModelCall,
            DuplicateCallId,
            BeyondBudget,
            DraftTooLarge,
            BadVersion,
            DraftBeforeReady,
            OversizePrompt,
        ]
    }
}

/// A misbehaving engine, as the `main` of an in-process `TestSandbox`.
pub struct HostileEngine {
    what: Hostile,
}

fn raw(w: &mut PipeWriter, bytes: &[u8]) {
    let _ = w.write_all(bytes);
}

fn ready_line(v: u32) -> Vec<u8> {
    format!("{{\"t\":\"ready\",\"engine_id\":\"hostile\",\"v\":{v}}}\n").into_bytes()
}

fn model_call_line(call_id: u64, task: u64, system_len: usize) -> Vec<u8> {
    format!(
        "{{\"t\":\"model_call\",\"call_id\":{call_id},\"task_id\":{task},\"purpose\":\"plan\",\"system\":\"{}\",\"user\":\"{{}}\",\"schema\":\"plan_v1\"}}\n",
        "s".repeat(system_len)
    )
    .into_bytes()
}

impl HostileEngine {
    pub fn new(what: Hostile) -> Self {
        Self { what }
    }

    /// The child `main`: read `Hello` and `PlanTask`, then misbehave.
    pub fn into_main(self) -> InProcessMain {
        let what = self.what;
        Arc::new(move |input: PipeReader, mut out: PipeWriter| run_hostile(what, input, &mut out))
    }
}

fn run_hostile(what: Hostile, input: PipeReader, out: &mut PipeWriter) {
    let mut reader = LineReader::new(input);
    let mut task = 0u64;
    // Hello then PlanTask.
    for _ in 0..2 {
        let Ok(Some(l)) = reader.next_line() else {
            return;
        };
        if let Ok(CoreToEngine::PlanTask(p)) = Codec::decode_core(&l) {
            task = p.task_id;
        }
    }
    use Hostile::*;
    if what == DraftBeforeReady {
        raw(
            out,
            format!("{{\"t\":\"plan_draft\",\"task_id\":{task},\"json\":\"{{}}\"}}\n").as_bytes(),
        );
        wait_closed(&mut reader);
        return;
    }
    if what == BadVersion {
        raw(out, &ready_line(PROTOCOL_VERSION + 1));
        wait_closed(&mut reader);
        return;
    }
    raw(out, &ready_line(PROTOCOL_VERSION));
    match what {
        ForgedApprove => raw(out, b"{\"t\":\"approve\",\"approval_id\":1}\n"),
        ForgedExecute => raw(out, b"{\"t\":\"execute\",\"action\":\"click\"}\n"),
        ForgedObserve => raw(out, b"{\"t\":\"observe\"}\n"),
        ForgedHello => raw(out, b"{\"t\":\"hello\",\"v\":1,\"session\":1}\n"),
        UnknownField => raw(
            out,
            format!(
                "{{\"t\":\"plan_draft\",\"task_id\":{task},\"json\":\"{{}}\",\"approved\":true}}\n"
            )
            .as_bytes(),
        ),
        OversizeLine => {
            let mut l = vec![b'x'; MAX_LINE + 10];
            l.push(b'\n');
            raw(out, &l);
        }
        EndlessLine => {
            for _ in 0..4 {
                raw(out, &vec![b'x'; MAX_LINE]);
            }
        }
        ThousandMessages => {
            for i in 0..1000u64 {
                raw(out, &model_call_line(i + 1, task, 1));
            }
        }
        InvalidUtf8 => raw(out, b"{\"t\":\"ready\",\"engine_id\":\"\xff\xfe\"}\n"),
        NulByte => raw(
            out,
            b"{\"t\":\"cannot_plan\",\"task_id\":1,\"code\":\"a\0b\"}\n",
        ),
        EmptyLine => raw(out, b"\n"),
        WrongTaskDraft => raw(
            out,
            format!(
                "{{\"t\":\"plan_draft\",\"task_id\":{},\"json\":\"{{}}\"}}\n",
                task + 1
            )
            .as_bytes(),
        ),
        WrongTaskModelCall => raw(out, &model_call_line(1, task + 1, 1)),
        DuplicateCallId => {
            for _ in 0..2 {
                raw(out, &model_call_line(7, task, 1));
                // Wait for the answer so the second call is the only violation.
                let _ = reader.next_line();
            }
        }
        BeyondBudget => {
            for i in 0..5u64 {
                raw(out, &model_call_line(i + 1, task, 1));
                let _ = reader.next_line();
            }
        }
        DraftTooLarge => raw(
            out,
            format!(
                "{{\"t\":\"plan_draft\",\"task_id\":{task},\"json\":\"{}\"}}\n",
                "j".repeat(20 * 1024)
            )
            .as_bytes(),
        ),
        OversizePrompt => raw(out, &model_call_line(1, task, 7 * 1024)),
        Hang | BadVersion | DraftBeforeReady => {}
    }
    wait_closed(&mut reader);
}

/// Block until core closes our stdin (kill or shutdown); everything received is ignored.
fn wait_closed(reader: &mut LineReader<PipeReader>) {
    while matches!(reader.next_line(), Ok(Some(_))) {}
}

/// A well-behaved child over the real protocol, for in-process tests: `MinimalEngine` as `main`.
pub fn minimal_main() -> InProcessMain {
    Arc::new(|input: PipeReader, out: PipeWriter| {
        let _ = crate::child::run_engine_main(input, out);
    })
}
