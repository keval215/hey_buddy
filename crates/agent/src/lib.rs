//! hey_buddy `agent` crate (ADR 0003 sections 2 to 4; docs/agent.md).
//!
//! The planner side of plan-then-execute: a tiny in-house engine ([`MinimalEngine`]) that runs as
//! a SANDBOXED child process (`buddy-agent`) and can only PROPOSE a JSON plan. Core talks to it
//! over stdio JSON lines ([`protocol`]); its only way to a model is core's [`ModelGateway`].
//!
//! # Isolation by construction
//! This crate depends on `serde`, `serde_json`, `thiserror`, `async-trait` and `tokio` only (plus
//! the `windows` crate for the Windows sandbox, target-gated). It does NOT depend on `policy`,
//! `driver`, `action`, `capture`, `firewall`, `llm`, `voice` or any network crate (CI cargo-tree
//! greps), and nothing below `core` may depend on it. The engine therefore has no path to the
//! gate, the driver or the firewall, and the protocol has no message that approves, executes or
//! observes. `PlanInput` has no field for screen text, titles, labels or observations.
//!
//! Errors, `Debug` and logs are fixed tokens: the transcript, prompts, model text and plans are
//! never printed.

pub mod child;
pub mod minimal;
pub mod process;
pub mod protocol;
pub mod sandbox;
pub mod schema;
mod types;

#[cfg(test)]
mod protocol_tests;

#[cfg(feature = "fakes")]
pub mod fakes;

pub use minimal::MinimalEngine;
pub use process::ProcessEngine;
pub use sandbox::{
    platform_sandbox, EngineProcess, EngineSandbox, KillHandle, LaunchSpec, ProcessStreams,
    SandboxError, UnsupportedSandbox,
};
pub use schema::{plan_v1_schema, PLAN_V1_SCHEMA_JSON};
pub use types::*;
