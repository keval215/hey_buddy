//! The engine process' `main` (used by the `buddy-agent` binary and by in-process tests): reads
//! core's lines, runs [`MinimalEngine`] with a stdio-backed [`ModelGateway`] (every model call is
//! a `ModelCall` line to core, answered by a `ModelResult` line), writes the draft.
//!
//! No file, network or process code exists here: the only I/O is the two streams it is given.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::minimal::MinimalEngine;
use crate::protocol::{
    Codec, CoreToEngine, EngineToCore, LineReader, PlanTaskWire, PROTOCOL_VERSION,
};
use crate::types::{GatewayError, ModelCall, ModelGateway, PlanOutcome};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why the child stopped (the exit code of `buddy-agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    /// Draft or `CannotPlan` delivered.
    Done,
    /// Core cancelled, shut down or closed the pipe.
    Cancelled,
    /// A protocol or I/O error.
    Failed,
}

struct StdioGateway<R: Read, W: Write> {
    reader: Mutex<LineReader<R>>,
    writer: Mutex<W>,
    next_id: AtomicU64,
}

fn send<W: Write>(w: &Mutex<W>, m: &EngineToCore) -> Result<(), ()> {
    let line = Codec::encode_engine(m).map_err(|_| ())?;
    let mut w = lock(w);
    w.write_all(&line).and_then(|_| w.flush()).map_err(|_| ())
}

#[async_trait]
impl<R: Read + Send, W: Write + Send> ModelGateway for StdioGateway<R, W> {
    async fn complete(&self, call: ModelCall) -> Result<String, GatewayError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        send(&self.writer, &EngineToCore::model_call(id, &call))
            .map_err(|_| GatewayError::Killed)?;
        // Blocking read on the single-threaded runtime: nothing else runs meanwhile, and core
        // enforces the deadline by killing this process.
        let mut r = lock(&self.reader);
        let line = match r.next_line() {
            Ok(Some(l)) => l,
            _ => return Err(GatewayError::Killed),
        };
        match Codec::decode_core(&line) {
            Ok(CoreToEngine::ModelResult { call_id, result }) if call_id == id => {
                result.into_result()
            }
            Ok(CoreToEngine::Cancel { .. } | CoreToEngine::Shutdown {}) => {
                Err(GatewayError::Killed)
            }
            _ => Err(GatewayError::Model("protocol")),
        }
    }
}

/// Run one task over `input`/`output` and return why we stopped.
pub fn run_engine_main<R: Read + Send, W: Write + Send>(input: R, output: W) -> ChildExit {
    let mut reader = LineReader::new(input);
    let writer = Mutex::new(output);
    // 1. Hello
    match next_core(&mut reader) {
        Some(CoreToEngine::Hello { v, .. }) if v == PROTOCOL_VERSION => {}
        _ => return ChildExit::Failed,
    }
    let ready = EngineToCore::Ready {
        engine_id: MinimalEngine::ID.to_string(),
        v: PROTOCOL_VERSION,
    };
    if send(&writer, &ready).is_err() {
        return ChildExit::Failed;
    }
    // 2. PlanTask
    let wire: PlanTaskWire = match next_core(&mut reader) {
        Some(CoreToEngine::PlanTask(p)) => p,
        Some(CoreToEngine::Cancel { .. } | CoreToEngine::Shutdown {}) | None => {
            return ChildExit::Cancelled
        }
        _ => return ChildExit::Failed,
    };
    let task_id = wire.task_id;
    let Ok(plan_input) = wire.into_input() else {
        return ChildExit::Failed;
    };
    let gateway = StdioGateway {
        reader: Mutex::new(reader),
        writer,
        next_id: AtomicU64::new(1),
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    else {
        return ChildExit::Failed;
    };
    let engine = MinimalEngine::new();
    let outcome = rt.block_on(engine.plan_with(&plan_input, &gateway));
    let reply = match outcome {
        Ok(PlanOutcome::Draft(d)) => EngineToCore::PlanDraft {
            task_id,
            json: d.into_string(),
        },
        Ok(PlanOutcome::CannotPlan(code)) => EngineToCore::CannotPlan {
            task_id,
            code: code.to_string(),
        },
        Err(crate::types::EngineError::Gateway(GatewayError::Killed)) => {
            return ChildExit::Cancelled
        }
        // Core already knows why the model call failed (it refused it); just stop.
        Err(_) => return ChildExit::Failed,
    };
    if send(&gateway.writer, &reply).is_err() {
        return ChildExit::Failed;
    }
    ChildExit::Done
}

fn next_core<R: Read>(r: &mut LineReader<R>) -> Option<CoreToEngine> {
    let line = r.next_line().ok()??;
    Codec::decode_core(&line).ok()
}
