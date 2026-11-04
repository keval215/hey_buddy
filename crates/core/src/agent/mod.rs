//! The agent supervisor (S2-17 + S2-23): voice request -> plan -> approval -> policy gate ->
//! driver -> audit, with the kill switch live. See `docs/supervisor.md`.

pub mod port;
pub mod supervisor;
pub mod types;

pub use port::{
    peek, AgentDriver, BindExtras, BindFacts, DriverRelease, ObservationSource,
    ReverifiedAtExecute, UnknownFacts,
};
pub use supervisor::{AgentSupervisor, SupervisorParts};
pub use types::*;

#[cfg(test)]
mod tests;
