//! The app-facing glue of the agent (S2-23b): [`AgentRuntime`] builds the supervisor from a
//! [`Buddy`], runs tasks on the async runtime, turns the [`TaskReport`] into a typed event
//! ([`AgentTaskDto`]) and speaks a short fixed phrase.
//!
//! What is spoken, exactly: one of the fixed sentences of `spoken::Notice` ("Done.", "Stopped.",
//! "I couldn't do that.", ...), or, for a `Completed` task with read results, the sentences of
//! its [`ReadAnswer`]s. Those contain one firewall-cleared label each and nothing else: never
//! model text, never the transcript, never other screen text. Speech goes through the buddy's
//! LOCAL TTS slot only; with no voice engine attached nothing is spoken.
//!
//! D-009: [`platform_agent`] is the only non-test place that picks the `BindFacts`. On Windows it
//! picks `ReverifiedAtExecute` together with the real driver from `windows_driver()`; everywhere
//! else it picks `UnknownFacts`, so the agent can only read.

use std::path::Path;
use std::sync::Arc;

use agent::{EngineSandbox, LaunchSpec};
use async_trait::async_trait;
use firewall::Firewall;
use futures::channel::mpsc as fmpsc;
use futures::StreamExt;
use policy::Catalogue;
use serde::Serialize;
use ts_rs::TS;
use voice::TtsOptions;

use super::port::{AgentDriver, BindFacts};
use super::supervisor::{AgentSupervisor, SupervisorError, SupervisorParts};
use super::types::{
    BuddyAgentSettings, BuddyDeny, EngineFactory, ProcessEngineFactory, TaskOutcome, TaskReport,
    TaskRequest,
};
use crate::approval::ApprovalBroker;
use crate::buddy::Buddy;
use crate::kill::KillController;
use crate::spoken::{self, Locale, Notice};
use crate::turn::ProviderSource;

/// Tauri event: how an agent task ended (settings window only).
pub const EVT_AGENT_TASK: &str = "agent:task_done";

// ---------------------------------------------------------------------------------------------
// DTOs (fixed tokens and counts; the answers carry the cleared labels they are made of)
// ---------------------------------------------------------------------------------------------

/// Coarse kind of a task outcome (`TaskOutcome::kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentOutcomeKind {
    Refused,
    PlanFailed,
    PlanRejected,
    Denied,
    Expired,
    Killed,
    Paused,
    Blocked,
    Failed,
    Stopped,
    Completed,
}

impl From<TaskOutcome> for AgentOutcomeKind {
    fn from(o: TaskOutcome) -> Self {
        match o {
            TaskOutcome::Refused(_) => Self::Refused,
            TaskOutcome::PlanFailed(_) => Self::PlanFailed,
            TaskOutcome::PlanRejected(_) => Self::PlanRejected,
            TaskOutcome::Denied(_) => Self::Denied,
            TaskOutcome::Expired(_) => Self::Expired,
            TaskOutcome::Killed(_) => Self::Killed,
            TaskOutcome::Paused(_) => Self::Paused,
            TaskOutcome::Blocked(_) => Self::Blocked,
            TaskOutcome::Failed(_) => Self::Failed,
            TaskOutcome::Stopped(_) => Self::Stopped,
            TaskOutcome::Completed => Self::Completed,
        }
    }
}

/// One read result (`find_text` / `answer_from_screen`): a count and the first matching cleared
/// label. `Debug` prints counts only.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct ReadAnswerDto {
    /// `find_text` or `answer_from_screen`.
    pub kind: String,
    pub matches: u16,
    pub first_role: String,
    pub first_label: String,
}

impl std::fmt::Debug for ReadAnswerDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReadAnswerDto({}, {} matches)", self.kind, self.matches)
    }
}

/// `agent:task_done`.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct AgentTaskDto {
    pub task: u32,
    pub outcome: AgentOutcomeKind,
    /// Fixed detail token (`ok`, `agent_disabled`, `outside_plan`, `step_approval`, ...).
    pub detail: String,
    pub dry_run: bool,
    pub would_do: u16,
    pub steps_done: u16,
    pub executed: u16,
    pub live: bool,
    pub answers: Vec<ReadAnswerDto>,
}

impl std::fmt::Debug for AgentTaskDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AgentTaskDto(task {}, {:?}/{}, {} answers)",
            self.task,
            self.outcome,
            self.detail,
            self.answers.len()
        )
    }
}

impl From<&TaskReport> for AgentTaskDto {
    fn from(r: &TaskReport) -> Self {
        Self {
            task: u32::try_from(r.task).unwrap_or(u32::MAX),
            outcome: r.outcome.into(),
            detail: r.outcome.detail().to_owned(),
            dry_run: r.dry_run,
            would_do: r.would_do,
            steps_done: r.steps_done,
            executed: r.executed,
            live: r.live,
            answers: r
                .answers
                .iter()
                .map(|a| ReadAnswerDto {
                    kind: a.kind().code().to_owned(),
                    matches: a.matches(),
                    first_role: a.first_role().to_owned(),
                    first_label: a.first_label().to_owned(),
                })
                .collect(),
        }
    }
}

/// `agent_status` command result. Fixed tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct AgentStatusDto {
    /// The agent can start a task now.
    pub ready: bool,
    /// A task is running.
    pub running: bool,
    pub observe_mode: bool,
    pub agent_mode: bool,
    /// Why not ready: `setting_off`, `not_ready` (activity log or stop shortcut) or `no_runtime`.
    pub reason: Option<String>,
}

impl AgentStatusDto {
    /// The status when no runtime was built (no platform support, or startup failed).
    pub fn no_runtime(b: &Buddy) -> Self {
        let s = b.get_settings();
        Self {
            ready: false,
            running: false,
            observe_mode: s.observe_mode,
            agent_mode: s.agent_mode,
            reason: Some("no_runtime".into()),
        }
    }
}

/// Why `start_task` did not start. Fixed tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// Not enabled: the setting is off, or the activity log / stop shortcut is not ready.
    Disabled,
    /// A task is already running.
    Busy,
    /// Empty request.
    BadRequest,
}

impl StartError {
    pub fn code(self) -> &'static str {
        match self {
            StartError::Disabled => "agent_disabled",
            StartError::Busy => "busy",
            StartError::BadRequest => "bad_request",
        }
    }
}

/// What `Buddy` hands a transcript to when `agent_mode` is on (set by `AgentRuntime::install`).
pub trait AgentTrigger: Send + Sync {
    fn start(&self, transcript: String) -> Result<(), StartError>;
}

// ---------------------------------------------------------------------------------------------
// Speaking
// ---------------------------------------------------------------------------------------------

/// Says one sentence out loud. Callers pass only fixed sentences or `ReadAnswer::sentence()`.
#[async_trait]
pub trait AgentVoice: Send + Sync {
    async fn say(&self, text: String);
}

/// The buddy's local TTS slot and audio output. Silent when no voice engine is attached.
pub struct BuddyVoice(pub Arc<Buddy>);

#[async_trait]
impl AgentVoice for BuddyVoice {
    async fn say(&self, text: String) {
        let Some(tts) = self.0.tts_slot().get() else {
            return;
        };
        let Some(deps) = self.0.engine_deps() else {
            return;
        };
        let (tx, rx) = fmpsc::unbounded::<String>();
        let Ok(audio) = tts.synthesize(rx.boxed(), TtsOptions::default()).await else {
            return;
        };
        let Ok(playback) = deps.audio_out.play(audio) else {
            return;
        };
        let _ = tx.unbounded_send(text);
        drop(tx);
        playback.finished().await;
    }
}

/// The sentences to speak for a finished task. Fixed sentences, or read answers (cleared labels).
/// A kill and a limit stop say nothing here: both go through the kill controller (`trip` /
/// `trip_limit`), which already said "Stopped." once.
pub fn sentences_for(report: &TaskReport) -> Vec<String> {
    let fixed = |n: Notice| vec![spoken::notice(n, Locale::En).to_owned()];
    match report.outcome {
        TaskOutcome::Completed => {
            if report.answers.is_empty() {
                fixed(Notice::AgentDone)
            } else {
                report.answers.iter().map(|a| a.sentence()).collect()
            }
        }
        TaskOutcome::Killed(_) | TaskOutcome::Stopped(_) => Vec::new(),
        TaskOutcome::Denied(_) | TaskOutcome::Expired(_) => fixed(Notice::AgentDeclined),
        TaskOutcome::Paused(_) => fixed(Notice::AgentNeedsYou),
        TaskOutcome::Refused(_)
        | TaskOutcome::PlanFailed(_)
        | TaskOutcome::PlanRejected(_)
        | TaskOutcome::Blocked(_)
        | TaskOutcome::Failed(_) => fixed(Notice::AgentCouldNot),
    }
}

// ---------------------------------------------------------------------------------------------
// Platform parts
// ---------------------------------------------------------------------------------------------

/// What the platform provides for the agent.
pub struct PlatformAgent {
    pub driver: Arc<dyn AgentDriver>,
    /// D-009: `ReverifiedAtExecute` only with the real Windows driver, else `UnknownFacts`.
    pub facts: Arc<dyn BindFacts>,
    pub sandbox: Arc<dyn EngineSandbox>,
    pub spec: LaunchSpec,
    pub catalogue: Arc<dyn Catalogue + Send + Sync>,
}

/// The fixed app list the planner may name (a Start-menu catalogue is a follow-up). Only Notepad.
pub fn builtin_catalogue() -> Arc<dyn Catalogue + Send + Sync> {
    Arc::new(
        policy::StaticCatalogue::new(&[("Notepad", "notepad", "notepad.exe")]).unwrap_or_default(),
    )
}

/// Windows: the real driver (`windows_driver()` refuses to start when this process is elevated),
/// `ReverifiedAtExecute`, the AppContainer sandbox and `<install_dir>\buddy-agent.exe`.
/// Compile-checked only on this machine (Linux).
#[cfg(windows)]
pub fn platform_agent(
    install_dir: &Path,
    firewall: Arc<dyn Firewall>,
) -> Result<PlatformAgent, &'static str> {
    let drv = driver::windows_driver(
        firewall,
        driver::LaunchAllowlist::windows_default(),
        Arc::new(policy::SystemClock::new()),
    )
    .map_err(|e| e.code())?;
    Ok(PlatformAgent {
        driver: Arc::new(super::port::WindowsAgentDriver::new(drv)),
        facts: Arc::new(super::port::ReverifiedAtExecute),
        sandbox: agent::platform_sandbox(),
        spec: LaunchSpec {
            exe: install_dir.join("buddy-agent.exe"),
            install_dir: install_dir.to_path_buf(),
        },
        catalogue: builtin_catalogue(),
    })
}

/// Not Windows: observation works (the platform firewall), no input is possible
/// (`UnsupportedActuator`), every fact is unknown (`UnknownFacts`: only reads can run) and the
/// sandbox is `UnsupportedSandbox` (`Refused(sandbox_unavailable)`).
#[cfg(not(windows))]
pub fn platform_agent(
    install_dir: &Path,
    firewall: Arc<dyn Firewall>,
) -> Result<PlatformAgent, &'static str> {
    Ok(PlatformAgent {
        driver: Arc::new(driver::Driver::new(firewall, driver::UnsupportedActuator)),
        facts: Arc::new(super::port::UnknownFacts),
        sandbox: agent::platform_sandbox(),
        spec: LaunchSpec {
            exe: install_dir.join("buddy-agent"),
            install_dir: install_dir.to_path_buf(),
        },
        catalogue: builtin_catalogue(),
    })
}

// ---------------------------------------------------------------------------------------------
// The runtime
// ---------------------------------------------------------------------------------------------

pub struct AgentRuntime {
    buddy: Arc<Buddy>,
    supervisor: Arc<AgentSupervisor>,
    settings: Arc<BuddyAgentSettings>,
    voice: Arc<dyn AgentVoice>,
    handle: tokio::runtime::Handle,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AgentRuntime({:?})", self.supervisor)
    }
}

impl AgentRuntime {
    /// The real wiring from a `Buddy`: its settings, its deny list, its audit sink (through the
    /// controller's gate) and a `ProcessEngineFactory` over the platform sandbox. The kill
    /// controller, the broker and the platform parts come from the shell.
    pub fn from_buddy(
        buddy: &Arc<Buddy>,
        ctl: Arc<KillController>,
        broker: ApprovalBroker,
        platform: PlatformAgent,
        providers: Arc<dyn ProviderSource>,
        handle: tokio::runtime::Handle,
    ) -> Result<Self, SupervisorError> {
        let factory = Arc::new(ProcessEngineFactory {
            buddy: Arc::clone(buddy),
            providers,
            sandbox: platform.sandbox,
            spec: platform.spec,
            session: 1,
        });
        Self::with_factory(
            buddy,
            ctl,
            broker,
            platform.driver,
            platform.facts,
            factory,
            platform.catalogue,
            handle,
        )
    }

    /// Like [`from_buddy`](Self::from_buddy) with a given engine factory (tests use a
    /// `TestSandbox` or a fake engine).
    #[allow(clippy::too_many_arguments)]
    pub fn with_factory(
        buddy: &Arc<Buddy>,
        ctl: Arc<KillController>,
        broker: ApprovalBroker,
        driver: Arc<dyn AgentDriver>,
        facts: Arc<dyn BindFacts>,
        factory: Arc<dyn EngineFactory>,
        catalogue: Arc<dyn Catalogue + Send + Sync>,
        handle: tokio::runtime::Handle,
    ) -> Result<Self, SupervisorError> {
        let settings = Arc::new(BuddyAgentSettings::new(Arc::clone(buddy)));
        let mut parts = SupervisorParts::new(
            ctl,
            buddy.audit_sink(),
            broker,
            driver,
            factory,
            settings.clone(),
            Arc::new(BuddyDeny(Arc::clone(buddy))),
            catalogue,
        );
        parts.facts = facts;
        let supervisor = Arc::new(AgentSupervisor::new(parts)?);
        // One trip stops the engine, the driver and the run loop.
        supervisor.wire_kill();
        Ok(Self {
            buddy: Arc::clone(buddy),
            supervisor,
            settings,
            voice: Arc::new(BuddyVoice(Arc::clone(buddy))),
            handle,
        })
    }

    /// Replaces how sentences are spoken (tests).
    pub fn with_voice(mut self, voice: Arc<dyn AgentVoice>) -> Self {
        self.voice = voice;
        self
    }

    /// Makes `agent_mode` voice requests reach this runtime.
    pub fn install(self: &Arc<Self>) {
        self.buddy
            .set_agent_trigger(Arc::new(Trigger(self.clone())));
    }

    pub fn supervisor(&self) -> &Arc<AgentSupervisor> {
        &self.supervisor
    }

    pub fn status(&self) -> AgentStatusDto {
        use super::types::AgentSettings;
        let s = self.buddy.get_settings();
        let reason = if !s.agent_enabled {
            Some("setting_off")
        } else if !self.buddy.agent_enabled() {
            Some("not_ready")
        } else {
            None
        };
        AgentStatusDto {
            ready: reason.is_none() && self.settings.agent_enabled(),
            running: self.supervisor.is_running(),
            observe_mode: s.observe_mode,
            agent_mode: s.agent_mode,
            reason: reason.map(str::to_owned),
        }
    }

    /// Starts one interactive task on the async runtime and returns at once. The outcome arrives
    /// as `EventSink::agent_task` and is spoken as a fixed phrase. Rejected (nothing started)
    /// when the agent is not enabled, busy, or the request is empty.
    pub fn start_task(self: &Arc<Self>, transcript: String) -> Result<(), StartError> {
        use super::types::AgentSettings;
        if !self.settings.agent_enabled() {
            return Err(StartError::Disabled);
        }
        if transcript.trim().is_empty() {
            return Err(StartError::BadRequest);
        }
        if self.supervisor.is_running() {
            return Err(StartError::Busy);
        }
        let me = Arc::clone(self);
        drop(self.handle.spawn(async move {
            let _ = me.run(transcript).await;
        }));
        Ok(())
    }

    /// Runs one task to its end, emits the DTO and speaks. (Public for tests and for callers that
    /// already are on the runtime.)
    pub async fn run(&self, transcript: String) -> TaskReport {
        let report = self
            .supervisor
            .run_task(TaskRequest::interactive(transcript))
            .await;
        self.buddy.sink().agent_task(AgentTaskDto::from(&report));
        for s in sentences_for(&report) {
            self.voice.say(s).await;
        }
        report
    }
}

/// What `Buddy` holds: the runtime behind the `AgentTrigger` trait.
struct Trigger(Arc<AgentRuntime>);

impl AgentTrigger for Trigger {
    fn start(&self, transcript: String) -> Result<(), StartError> {
        self.0.start_task(transcript)
    }
}
