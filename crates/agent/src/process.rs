//! `ProcessEngine`: core's side of the stdio protocol. One sandboxed `buddy-agent` process per
//! task. Everything the engine says is untrusted: every line is size-, UTF-8-, NUL- and
//! schema-checked, counted (<= 64 messages), checked for task identity and unique call ids; ANY
//! violation, a deadline, a kill or dropping the future ends with the process KILLED. The engine
//! only ever receives `Hello`, `PlanTask`, `ModelResult`, `Cancel` and `Shutdown`: there is no
//! message that carries an observation or a tool result, and nothing is sent after the draft
//! except `Shutdown`.
//!
//! `kill()` never blocks: it flips a flag, signals the running `plan` and calls the sandbox's kill
//! handle (Windows: `TerminateJobObject`). After `kill()` this instance is dead.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::protocol::{
    schema_from_name, Codec, CoreToEngine, EngineMessageTracker, EngineToCore, LineReader,
    ModelOutcome, PlanTaskWire, ProtocolError, PROTOCOL_VERSION,
};
use crate::sandbox::{EngineProcess, EngineSandbox, KillHandle, LaunchSpec};
use crate::types::{
    AgentEngine, EngineError, GatewayError, ModelCall, ModelGateway, PlanDraftJson, PlanInput,
    PlanOutcome, TaskId,
};

/// `CannotPlan` codes the engine may report (closed set mapped to `&'static str`).
const CANNOT_CODES: &[&str] = &[
    "cannot_plan",
    "invalid_plan",
    "empty_request",
    "input_too_large",
    "unsupported",
];

type Line = Result<Option<Vec<u8>>, ProtocolError>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct KillState {
    killed: AtomicBool,
    tx: watch::Sender<bool>,
    handle: Mutex<Option<KillHandle>>,
}

pub struct ProcessEngine {
    sandbox: Arc<dyn EngineSandbox>,
    spec: LaunchSpec,
    session: u64,
    state: Arc<KillState>,
    busy: Arc<AtomicBool>,
}

impl ProcessEngine {
    pub const ID: &'static str = "buddy-agent";

    pub fn new(sandbox: Arc<dyn EngineSandbox>, spec: LaunchSpec, session: u64) -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            sandbox,
            spec,
            session,
            state: Arc::new(KillState {
                killed: AtomicBool::new(false),
                tx,
                handle: Mutex::new(None),
            }),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A cloneable, non-blocking kill closure (the supervisor/kill switch keeps one).
    pub fn killer(&self) -> KillHandle {
        let st = self.state.clone();
        Arc::new(move || st.kill())
    }
}

impl KillState {
    fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        self.tx.send_replace(true);
        // Clone out of the lock so the (non-blocking) handle never runs under it.
        let h = lock(&self.handle).clone();
        if let Some(h) = h {
            h();
        }
    }
}

/// Owns the running process for one `plan` call. Dropping it (success, error, timeout or the
/// caller dropping the future) cancels, kills and reaps. Nothing here blocks.
struct Session {
    kill: KillHandle,
    process: Mutex<Option<Box<dyn EngineProcess>>>,
    writer: std_mpsc::Sender<Vec<u8>>,
    task: TaskId,
    finished: bool,
    _busy: BusyGuard,
}

/// Resets the `busy` flag on every exit path (also when the `plan` future is dropped while the
/// sandbox is still spawning).
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

type Spawned = Result<(Box<dyn EngineProcess>, KillHandle), crate::sandbox::SandboxError>;

impl Session {
    fn send(&self, m: &CoreToEngine) -> Result<(), ProtocolError> {
        let line = Codec::encode_core(m)?;
        self.writer.send(line).map_err(|_| ProtocolError::Io)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.finished {
            // Best effort; the engine may ignore it, the kill below does not depend on it.
            let _ = self.send(&CoreToEngine::Cancel {
                task_id: self.task.0,
            });
        }
        (self.kill)();
        let taken = lock(&self.process).take();
        if let Some(mut p) = taken {
            // Reap off-thread: never wait for the engine.
            std::thread::spawn(move || {
                let _ = p.wait();
            });
        }
    }
}

fn spawn_io(
    streams: crate::sandbox::ProcessStreams,
) -> (std_mpsc::Sender<Vec<u8>>, mpsc::Receiver<Line>) {
    let crate::sandbox::ProcessStreams {
        mut stdin,
        stdout,
        stderr,
    } = streams;
    // Writer: lines in, bytes out; stops at the first error or when the channel closes.
    let (wtx, wrx) = std_mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(line) = wrx.recv() {
            if stdin.write_all(&line).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
    // Reader: strict lines into a small bounded channel (back-pressure stops a flood).
    let (rtx, rrx) = mpsc::channel::<Line>(8);
    std::thread::spawn(move || {
        let mut r = LineReader::new(stdout);
        loop {
            let item = r.next_line();
            let stop = !matches!(item, Ok(Some(_)));
            if rtx.blocking_send(item).is_err() || stop {
                break;
            }
        }
    });
    // stderr: drained so the child never blocks on it; kept nowhere (memory only, never logged).
    if let Some(mut e) = stderr {
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while matches!(e.read(&mut buf), Ok(n) if n > 0) {}
        });
    }
    (wtx, rrx)
}

use std::io::Read as _;

enum Step {
    Done(PlanOutcome),
    Continue,
}

impl ProcessEngine {
    async fn run(
        &self,
        input: &PlanInput,
        models: &Arc<dyn ModelGateway>,
        session: &Session,
        mut lines: mpsc::Receiver<Line>,
        deadline: Instant,
    ) -> Result<PlanOutcome, EngineError> {
        let mut killed_rx = self.state.tx.subscribe();
        let mut tracker = EngineMessageTracker::new(input.task_id);
        let mut calls: u8 = 0;
        loop {
            let line = tokio::select! {
                biased;
                _ = killed_rx.wait_for(|k| *k) => return Err(EngineError::Killed),
                _ = tokio::time::sleep_until(deadline) => return Err(EngineError::Timeout),
                l = lines.recv() => l,
            };
            let bytes = match line {
                Some(Ok(Some(b))) => b,
                Some(Ok(None)) | None => return Err(EngineError::Protocol("eof")),
                Some(Err(e)) => return Err(EngineError::Protocol(e.code())),
            };
            let msg = Codec::decode_engine(&bytes).map_err(|e| EngineError::Protocol(e.code()))?;
            tracker
                .accept(&msg)
                .map_err(|e| EngineError::Protocol(e.code()))?;
            match self
                .handle(
                    msg,
                    input,
                    models,
                    session,
                    &mut calls,
                    &mut killed_rx,
                    deadline,
                )
                .await?
            {
                Step::Done(o) => return Ok(o),
                Step::Continue => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle(
        &self,
        msg: EngineToCore,
        input: &PlanInput,
        models: &Arc<dyn ModelGateway>,
        session: &Session,
        calls: &mut u8,
        killed_rx: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Step, EngineError> {
        match msg {
            EngineToCore::Ready { .. } => Ok(Step::Continue),
            EngineToCore::ModelCall {
                call_id,
                task_id,
                purpose,
                system,
                user,
                schema,
            } => {
                *calls = calls.saturating_add(1);
                if *calls > input.limits.max_model_calls {
                    return Err(EngineError::LimitExceeded("model_calls"));
                }
                let schema = schema_from_name(&schema).ok_or(EngineError::Protocol("bad_field"))?;
                let call = ModelCall {
                    task_id: TaskId(task_id),
                    purpose: purpose.into_purpose(),
                    system,
                    user,
                    schema,
                };
                let res = tokio::select! {
                    biased;
                    _ = killed_rx.wait_for(|k| *k) => return Err(EngineError::Killed),
                    _ = tokio::time::sleep_until(deadline) => return Err(EngineError::Timeout),
                    r = models.complete(call) => r,
                };
                // Any refusal or failure ends the task: the engine cannot do better.
                let text = res.map_err(EngineError::Gateway)?;
                let mut reply = CoreToEngine::ModelResult {
                    call_id,
                    result: ModelOutcome::Text { text },
                };
                if Codec::encode_core(&reply).is_err() {
                    // Too large for one line once escaped: tell the engine, which gives up.
                    reply = CoreToEngine::ModelResult {
                        call_id,
                        result: ModelOutcome::Failed {
                            code: GatewayError::TooLarge.code().to_string(),
                        },
                    };
                }
                session
                    .send(&reply)
                    .map_err(|_| EngineError::Protocol("write_failed"))?;
                Ok(Step::Continue)
            }
            EngineToCore::PlanDraft { json, .. } => {
                let draft = PlanDraftJson::new(json)
                    .map_err(|_| EngineError::Protocol("field_too_large"))?;
                Ok(Step::Done(PlanOutcome::Draft(draft)))
            }
            EngineToCore::CannotPlan { code, .. } => {
                let code = CANNOT_CODES
                    .iter()
                    .find(|c| **c == code)
                    .ok_or(EngineError::Protocol("bad_code"))?;
                Ok(Step::Done(PlanOutcome::CannotPlan(code)))
            }
        }
    }
}

#[async_trait]
impl AgentEngine for ProcessEngine {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn plan(
        &self,
        input: PlanInput,
        models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError> {
        if self.state.killed.load(Ordering::SeqCst) {
            return Err(EngineError::Killed);
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(EngineError::LimitExceeded("busy"));
        }
        // From here on the guard resets `busy` on every exit path.
        let busy = BusyGuard(self.busy.clone());
        // `spawn` may block (Windows: profile, job and process creation), so it runs on the
        // blocking pool. It stores the kill handle itself and honours a kill that raced it. If this
        // future is dropped meanwhile, the closure kills and reaps the process it just made.
        let (tx, rx) = tokio::sync::oneshot::channel::<Spawned>();
        {
            let (sandbox, spec, state) =
                (self.sandbox.clone(), self.spec.clone(), self.state.clone());
            tokio::task::spawn_blocking(move || {
                let r: Spawned = sandbox.spawn(&spec).map(|p| {
                    let k = p.kill_handle();
                    *lock(&state.handle) = Some(k.clone());
                    (p, k)
                });
                if let Ok((_, k)) = &r {
                    if state.killed.load(Ordering::SeqCst) {
                        k();
                    }
                }
                if let Err(Ok((mut p, k))) = tx.send(r) {
                    k();
                    std::thread::spawn(move || {
                        let _ = p.wait();
                    });
                }
            });
        }
        let (mut process, kill) = match rx.await {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => return Err(e.to_engine_error()),
            Err(_) => return Err(EngineError::Unavailable("spawn_failed")),
        };
        let streams = process.take_streams();
        let (writer, lines) = match streams {
            Some(s) => spawn_io(s),
            None => {
                kill();
                return Err(EngineError::Unavailable("spawn_failed"));
            }
        };
        let session = Session {
            kill,
            process: Mutex::new(Some(process)),
            writer,
            task: input.task_id,
            finished: false,
            _busy: busy,
        };
        // A kill that raced the spawn: the handle was not stored yet when it ran.
        if self.state.killed.load(Ordering::SeqCst) {
            return Err(EngineError::Killed);
        }
        let deadline = Instant::now() + input.limits.deadline;
        let hello = CoreToEngine::Hello {
            v: PROTOCOL_VERSION,
            session: self.session,
        };
        let task = CoreToEngine::PlanTask(PlanTaskWire::from_input(&input));
        if session.send(&hello).and(session.send(&task)).is_err() {
            return Err(EngineError::Protocol("write_failed"));
        }
        let mut session = session;
        let out = self.run(&input, &models, &session, lines, deadline).await;
        if out.is_ok() {
            // Only Shutdown may follow a draft (threat row 5); then the process is killed on drop.
            let _ = session.send(&CoreToEngine::Shutdown {});
            session.finished = true;
        }
        out
    }

    fn kill(&self) {
        self.state.kill();
    }
}
