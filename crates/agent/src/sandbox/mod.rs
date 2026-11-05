//! Engine launcher (ADR 0003 section 3). `EngineSandbox::spawn` either returns a sandboxed child
//! speaking the stdio protocol or fails CLOSED: there is no unsandboxed fallback anywhere.
//!
//! * [`UnsupportedSandbox`]: every spawn fails with `unsupported_platform` (the agent stays off).
//! * `WindowsSandbox` (`cfg(windows)`, `windows.rs`): AppContainer with zero capabilities + Job
//!   Object. Compile-checked on Linux only; every decision that can be pure lives in [`spec`].
//! * `TestSandbox` (feature `fakes`, `test.rs`): in-memory pipes or a plain child process, for
//!   tests only; it never ships (CI checks the desktop tree has no `fakes`).
//!
//! This module (and `test.rs`) are the only places in the crate that may touch processes or the
//! file system (CI grep).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::types::EngineError;

pub mod spec;
#[cfg(feature = "fakes")]
mod test;
#[cfg(windows)]
mod windows;

#[cfg(feature = "fakes")]
pub use test::{pipe, InProcessMain, PipeReader, PipeWriter, TestSandbox};
#[cfg(windows)]
pub use windows::WindowsSandbox;

/// What to launch. On Windows `exe` must be `<install_dir>\buddy-agent.exe` (validated).
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub exe: PathBuf,
    pub install_dir: PathBuf,
}

/// Fixed tokens only (the OS error text is dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    #[error("unsupported_platform")]
    Unsupported,
    /// A sandbox step failed: nothing was spawned.
    #[error("sandbox_failed")]
    Failed(&'static str),
    /// The (test) child could not be started.
    #[error("spawn_failed")]
    Spawn,
}

impl SandboxError {
    pub fn to_engine_error(self) -> EngineError {
        EngineError::Unavailable(match self {
            SandboxError::Unsupported => "unsupported_platform",
            SandboxError::Failed(_) => "sandbox_failed",
            SandboxError::Spawn => "spawn_failed",
        })
    }
}

/// The three pipe ends the parent keeps.
pub struct ProcessStreams {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    /// Drained by the caller, capped, memory only, never logged or audited.
    pub stderr: Option<Box<dyn Read + Send>>,
}

pub type KillHandle = Arc<dyn Fn() + Send + Sync>;

pub trait EngineProcess: Send {
    /// The parent's pipe ends; `Some` once.
    fn take_streams(&mut self) -> Option<ProcessStreams>;
    /// Non-blocking and idempotent: terminates the whole process tree (Windows: the Job Object).
    fn kill_handle(&self) -> KillHandle;
    /// Blocks until the process is gone; the exit code if known. Call from a reaper thread.
    fn wait(&mut self) -> Option<i32>;
}

pub trait EngineSandbox: Send + Sync {
    fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError>;
}

/// Fails closed on every platform that has no sandbox implementation.
pub struct UnsupportedSandbox;

impl EngineSandbox for UnsupportedSandbox {
    fn spawn(&self, _spec: &LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError> {
        Err(SandboxError::Unsupported)
    }
}

/// The sandbox of this OS: `WindowsSandbox` on Windows, `UnsupportedSandbox` elsewhere.
pub fn platform_sandbox() -> Arc<dyn EngineSandbox> {
    #[cfg(windows)]
    {
        Arc::new(WindowsSandbox::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnsupportedSandbox)
    }
}
