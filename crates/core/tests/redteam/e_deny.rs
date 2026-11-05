//! Case e: a deny-listed app is in the foreground or named by the plan.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::rig::*;
use agent::{AgentEngine, EngineError, ModelGateway, PlanInput, PlanOutcome};
use async_trait::async_trait;
use buddy_core::agent::{
    DenyProvider, EngineFactory, EngineParts, RefuseReason, TaskOutcome, TaskRequest,
};
use frame::AppDenyPolicy;

/// What the planner was given: the foreground app, per call.
#[derive(Default)]
struct Seen(Mutex<Vec<Option<String>>>);

struct RecEngine(Arc<Seen>);
#[async_trait]
impl AgentEngine for RecEngine {
    fn id(&self) -> &str {
        "rec"
    }
    async fn plan(
        &self,
        input: PlanInput,
        _models: Arc<dyn ModelGateway>,
    ) -> Result<PlanOutcome, EngineError> {
        self.0
             .0
            .lock()
            .unwrap()
            .push(input.context.foreground_app.as_deref().map(str::to_owned));
        Err(EngineError::Cancelled)
    }
    fn kill(&self) {}
}

struct RecFactory(Arc<Seen>);
impl EngineFactory for RecFactory {
    fn create(&self, _t: agent::TaskId) -> Result<EngineParts, EngineError> {
        Ok(EngineParts {
            engine: Arc::new(RecEngine(self.0.clone())),
            gateway: Arc::new(NoModels),
            core_gateway: None,
        })
    }
}

/// Counts `is_denied` calls and denies exactly the call with index `at` (0-based), nothing else.
struct PointDeny {
    calls: AtomicUsize,
    at: Option<usize>,
}
impl PointDeny {
    fn new(at: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            at,
        })
    }
}
impl AppDenyPolicy for PointDeny {
    fn is_denied(&self, _a: &str) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) == self.at.unwrap_or(usize::MAX)
    }
}

/// Attack: the foreground app is deny-listed at the moment the supervisor builds the planner's
/// input, although the firewall's own observation was Clear (the list changed in between, or the
/// firewall's check and ours disagree). Expected: the planner gets NO foreground app.
/// Defence: `foreground_app` re-checks the deny list before the app id goes anywhere.
///
/// The fake is keyed on call identity with a FIXED expectation: `k` is how many times the
/// firewall asks the deny list during ONE observation (measured by observing directly, not through
/// the supervisor); the supervisor must make exactly one more ask, the foreground filter, before
/// the planner is called (`k + 1`), and denying exactly that ask must blank the app. Removing the
/// filter makes the count `k` and the app leak, so both assertions fail (mutation-checked, see
/// `docs/redteam.md`).
#[tokio::test]
async fn the_foreground_filter_blanks_a_deny_listed_app_that_the_firewall_let_through() {
    // Measure k with the firewall alone.
    let probe = PointDeny::new(None);
    let e0 = env();
    let _ = e0.drv.observe(&*probe);
    let k = probe.calls.load(Ordering::SeqCst);
    assert!(k >= 1, "the firewall consults the deny list");

    // Run 1: nothing denied. The planner learns the app; exactly one ask beyond the firewall's.
    let seen = Arc::new(Seen::default());
    let deny = PointDeny::new(None);
    let e = env_with(Opts {
        deny: deny.clone(),
        factory: Some(Arc::new(RecFactory(seen.clone()))),
        ..Opts::default()
    });
    let _ = e.run("unused").await;
    assert_eq!(
        deny.calls.load(Ordering::SeqCst),
        k + 1,
        "the supervisor asked the deny list once more (the foreground filter) before planning"
    );
    assert_eq!(seen.0.lock().unwrap()[0].as_deref(), Some("notes.exe"));

    // Run 2: deny exactly the (k+1)-th ask, the foreground filter.
    let seen = Arc::new(Seen::default());
    let deny = PointDeny::new(Some(k));
    let e = env_with(Opts {
        deny: deny.clone(),
        factory: Some(Arc::new(RecFactory(seen.clone()))),
        ..Opts::default()
    });
    let _ = e.run("unused").await;
    assert_eq!(
        seen.0.lock().unwrap()[0],
        None,
        "a deny-listed app must never reach the planner"
    );
}

/// Attack: the foreground app is deny-listed from the start. Expected: the planner gets no app,
/// the screen is withheld, the task is `Refused(observation_unavailable)`, nothing is offered or
/// run. Defence: firewall withholds a denied app; supervisor refuses without an observation.
#[tokio::test]
async fn a_deny_listed_foreground_is_refused_and_blank_for_the_planner() {
    let seen = Arc::new(Seen::default());
    let e = env_with(Opts {
        deny: Arc::new(Deny(&["notes.exe"])),
        factory: Some(Arc::new(RecFactory(seen.clone()))),
        ..Opts::default()
    });
    let r = e.run("unused").await;
    assert_eq!(seen.0.lock().unwrap()[0], None);
    assert!(matches!(r.outcome, TaskOutcome::PlanFailed(_)), "{r:?}");

    // With a real plan: refused at the dry run, nothing offered.
    let e = env_with(Opts {
        deny: Arc::new(Deny(&["notes.exe"])),
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Refused(RefuseReason::ObservationUnavailable),
        "{r:?}"
    );
    e.executed_nothing();
    assert!(e.human.seen().is_empty());
    assert!(
        e.screen_text().is_some(),
        "control: Deny(&[]) sees the screen"
    );
    e.gate_idle();
}

/// Attack: the plan names a deny-listed app (`open_app Notepad` while Notepad is on the deny
/// list). Expected: refused (never offered, nothing launched). Defence: `target_denied` from the
/// deny list makes the step Never.
#[tokio::test]
async fn a_plan_that_names_a_deny_listed_app_is_refused() {
    let e = env_with(Opts {
        deny: Arc::new(Deny(&["notepad.exe"])),
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert!(failed_closed(&r), "{r:?}");
    assert!(
        matches!(
            r.outcome,
            TaskOutcome::Refused(RefuseReason::PlanRefusedStep)
        ),
        "{r:?}"
    );
    e.executed_nothing();
    assert!(e.human.seen().is_empty());
}

/// Attack: the deny list cannot be read (None). Expected: `Refused(no_deny_list)`, no engine,
/// nothing observed. Defence: fail closed.
#[tokio::test]
async fn an_unavailable_deny_list_refuses_the_task() {
    struct NoList;
    impl DenyProvider for NoList {
        fn current(&self) -> Option<Arc<dyn AppDenyPolicy>> {
            None
        }
    }
    let e = env_with(Opts {
        deny_provider: Some(Arc::new(NoList)),
        ..Opts::default()
    });
    e.factory.draft(&notepad_plan());
    let r = bounded(e.sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::NoDenyList));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 0);
    e.executed_nothing();
}
