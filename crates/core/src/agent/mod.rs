//! The agent supervisor (S2-17 + S2-23): voice request -> plan -> approval -> policy gate ->
//! driver -> audit, with the kill switch live. See `docs/supervisor.md`.

pub mod port;
pub mod read;
pub mod runtime;
pub mod supervisor;
pub mod types;

pub use port::{
    peek, AgentDriver, BindExtras, BindFacts, DriverRelease, ObservationSource,
    ReverifiedAtExecute, UnknownFacts,
};
pub use read::{ReadAnswer, ReadKind, MAX_ANSWERS};
pub use runtime::{
    builtin_catalogue, platform_agent, sentences_for, AgentOutcomeKind, AgentRuntime,
    AgentStatusDto, AgentTaskDto, AgentTrigger, AgentVoice, BuddyVoice, PlatformAgent,
    ReadAnswerDto, StartError, EVT_AGENT_TASK,
};
pub use supervisor::{AgentSupervisor, SupervisorError, SupervisorParts};
pub use types::*;

#[cfg(test)]
mod tests;
