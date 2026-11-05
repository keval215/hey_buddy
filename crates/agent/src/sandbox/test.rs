//! `TestSandbox` (feature `fakes`): NO Windows APIs and NO isolation. It runs an engine either
//! in-process over in-memory pipes (a closure standing in for the child's `main`) or as a plain
//! child process through `std::process`. Tests only; never part of the shipped app.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::{EngineProcess, EngineSandbox, KillHandle, LaunchSpec, ProcessStreams, SandboxError};

struct PipeState {
    buf: VecDeque<u8>,
    closed: bool,
}

struct Shared {
    state: Mutex<PipeState>,
    cv: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct Pipe(Arc<Shared>);

impl Pipe {
    fn new() -> Self {
        Pipe(Arc::new(Shared {
            state: Mutex::new(PipeState {
                buf: VecDeque::new(),
                closed: false,
            }),
            cv: Condvar::new(),
        }))
    }
    fn close(&self) {
        lock(&self.0.state).closed = true;
        self.0.cv.notify_all();
    }
}

/// Reading end of an in-memory pipe: blocks until data, EOF once the pipe is closed and drained.
pub struct PipeReader(Pipe);
/// Writing end: dropping it closes the pipe (the reader sees EOF); writing to a closed pipe fails.
pub struct PipeWriter(Pipe);

/// A new in-memory pipe.
pub fn pipe() -> (PipeReader, PipeWriter) {
    let p = Pipe::new();
    (PipeReader(p.clone()), PipeWriter(p))
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut st = lock(&(self.0).0.state);
        loop {
            if !st.buf.is_empty() {
                let n = out.len().min(st.buf.len());
                for slot in out.iter_mut().take(n) {
                    *slot = st.buf.pop_front().unwrap_or(0);
                }
                return Ok(n);
            }
            if st.closed {
                return Ok(0);
            }
            st = (self.0).0.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut st = lock(&(self.0).0.state);
        if st.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        st.buf.extend(data.iter().copied());
        (self.0).0.cv.notify_all();
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The "child main" of an in-process engine: reads core's lines, writes the engine's lines.
pub type InProcessMain = Arc<dyn Fn(PipeReader, PipeWriter) + Send + Sync>;

enum Mode {
    InProcess(InProcessMain),
    Child(PathBuf),
}

pub struct TestSandbox {
    mode: Mode,
}

impl TestSandbox {
    /// Run `main` on a thread over in-memory pipes. `kill` closes both pipes.
    pub fn in_process(main: InProcessMain) -> Self {
        Self {
            mode: Mode::InProcess(main),
        }
    }

    /// Spawn the real `buddy-agent` binary at `exe` as a plain child process (stdin/stdout/stderr
    /// piped, EMPTY environment, no isolation: this is the protocol test on Linux).
    pub fn real_child(exe: impl Into<PathBuf>) -> Self {
        Self {
            mode: Mode::Child(exe.into()),
        }
    }
}

impl EngineSandbox for TestSandbox {
    fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError> {
        match &self.mode {
            Mode::InProcess(main) => {
                let (child_in, core_out) = pipe();
                let (core_in, child_out) = pipe();
                let done = Arc::new(AtomicBool::new(false));
                let (d2, main) = (done.clone(), main.clone());
                let (kin, kout) = (
                    Pipe(Arc::clone(&(child_in.0).0)),
                    Pipe(Arc::clone(&(child_out.0).0)),
                );
                std::thread::spawn(move || {
                    main(child_in, child_out);
                    d2.store(true, Ordering::SeqCst);
                });
                Ok(Box::new(InProcess {
                    streams: Some(ProcessStreams {
                        stdin: Box::new(core_out),
                        stdout: Box::new(core_in),
                        stderr: None,
                    }),
                    pipes: (kin, kout),
                    done,
                    killed: Arc::new(AtomicBool::new(false)),
                }))
            }
            Mode::Child(exe) => {
                let exe = if exe.as_os_str().is_empty() {
                    &spec.exe
                } else {
                    exe
                };
                let mut child = Command::new(exe)
                    .env_clear()
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|_| SandboxError::Spawn)?;
                let streams = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                    (Some(i), Some(o), Some(e)) => ProcessStreams {
                        stdin: Box::new(i),
                        stdout: Box::new(o),
                        stderr: Some(Box::new(e)),
                    },
                    _ => {
                        let _ = child.kill();
                        return Err(SandboxError::Spawn);
                    }
                };
                Ok(Box::new(RealChild {
                    streams: Some(streams),
                    child: Arc::new(Mutex::new(child)),
                }))
            }
        }
    }
}

struct InProcess {
    streams: Option<ProcessStreams>,
    pipes: (Pipe, Pipe),
    done: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
}

impl EngineProcess for InProcess {
    fn take_streams(&mut self) -> Option<ProcessStreams> {
        self.streams.take()
    }
    fn kill_handle(&self) -> KillHandle {
        let (a, b) = (self.pipes.0.clone(), self.pipes.1.clone());
        let killed = self.killed.clone();
        Arc::new(move || {
            killed.store(true, Ordering::SeqCst);
            a.close();
            b.close();
        })
    }
    fn wait(&mut self) -> Option<i32> {
        while !self.done.load(Ordering::SeqCst) && !self.killed.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Some(0)
    }
}

struct RealChild {
    streams: Option<ProcessStreams>,
    child: Arc<Mutex<Child>>,
}

impl EngineProcess for RealChild {
    fn take_streams(&mut self) -> Option<ProcessStreams> {
        self.streams.take()
    }
    fn kill_handle(&self) -> KillHandle {
        let child = self.child.clone();
        Arc::new(move || {
            // `Child::kill` is SIGKILL (non-blocking) and idempotent for an exited child.
            let _ = lock(&child).kill();
        })
    }
    fn wait(&mut self) -> Option<i32> {
        loop {
            match lock(&self.child).try_wait() {
                Ok(Some(s)) => return s.code(),
                Ok(None) => {}
                Err(_) => return None,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
