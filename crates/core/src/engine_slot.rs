//! `EngineSlot<T>`: holds the active voice engine (STT or TTS) and swaps it without a restart
//! (ticket S2-2).
//!
//! Rules:
//! - A replacement is loaded on its OWN thread, never on the audio thread or a tokio worker.
//! - The swap happens atomically and ONLY after the load succeeded. A failed (or panicked) load
//!   keeps the old engine; an empty slot stays empty (`get()` is `None`, "not loaded").
//! - A turn calls [`EngineSlot::begin_turn`] once at its start: it gets the `Arc` of the active
//!   engine and keeps it for the whole turn. While a turn is active a finished load waits in the
//!   pending slot and is applied by [`EngineSlot::end_turn`]. The newest request wins: asking
//!   again drops the older pending engine, and an older load that finishes late is discarded.
//! - Failures carry fixed tokens (and, for a missing model file, the expected path), never
//!   content.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};

use voice::{SttProvider, TtsProvider};

/// Fixed error token for a load that failed. `path` is the expected model file for
/// `model_missing`; nothing else is ever put in here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapFailure {
    pub token: &'static str,
    pub path: Option<String>,
}

impl SwapFailure {
    pub fn new(token: &'static str) -> Self {
        Self { token, path: None }
    }
    pub fn missing(path: impl Into<String>) -> Self {
        Self {
            token: TOKEN_MODEL_MISSING,
            path: Some(path.into()),
        }
    }
}

pub const TOKEN_MODEL_MISSING: &str = "model_missing";
pub const TOKEN_LOAD_FAILED: &str = "load_failed";

/// Builds the engines for a swap (the real adapters, or fakes in tests). Called on a
/// background thread; slow work is fine, content in errors is not.
pub trait EngineLoader: Send + Sync {
    fn load_stt(&self, model_id: &str) -> Result<Arc<dyn SttProvider>, SwapFailure>;
    fn load_tts(&self, model_id: &str) -> Result<Arc<dyn TtsProvider>, SwapFailure>;
}

/// What a slot reports to its observer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapNotice {
    /// A load was accepted and started.
    Requested { id: String },
    /// The new engine is now the active one.
    Done { id: String },
    /// The load failed; the old engine (if any) stays.
    Failed { id: String, failure: SwapFailure },
}

pub type SwapObserver = Arc<dyn Fn(SwapNotice) + Send + Sync>;

/// Loads one replacement engine. Runs on a background thread.
pub type Loader<T> = Box<dyn FnOnce() -> Result<Arc<T>, SwapFailure> + Send + 'static>;

struct State<T: ?Sized> {
    active: Option<(Arc<T>, String)>,
    pending: Option<(Arc<T>, String)>,
    busy: bool,
    /// Bumped by every request; a load whose generation is stale is discarded.
    generation: u64,
}

struct Shared<T: ?Sized> {
    state: Mutex<State<T>>,
    observer: SwapObserver,
}

pub struct EngineSlot<T: ?Sized + Send + Sync + 'static> {
    shared: Arc<Shared<T>>,
}

impl<T: ?Sized + Send + Sync + 'static> Clone for EngineSlot<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

fn lock<T: ?Sized>(m: &Mutex<State<T>>) -> MutexGuard<'_, State<T>> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<T: ?Sized + Send + Sync + 'static> EngineSlot<T> {
    /// An empty slot ("not loaded") reporting to `observer`.
    pub fn new(observer: SwapObserver) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    active: None,
                    pending: None,
                    busy: false,
                    generation: 0,
                }),
                observer,
            }),
        }
    }

    /// An empty slot nobody listens to.
    pub fn silent() -> Self {
        Self::new(Arc::new(|_| {}))
    }

    /// Install `engine` directly (no load, no events). Used by tests and fixed setups.
    pub fn set(&self, engine: Arc<T>, id: impl Into<String>) {
        let mut st = lock(&self.shared.state);
        st.generation += 1;
        st.pending = None;
        st.active = Some((engine, id.into()));
    }

    /// The active engine, if one is loaded.
    pub fn get(&self) -> Option<Arc<T>> {
        lock(&self.shared.state)
            .active
            .as_ref()
            .map(|(e, _)| e.clone())
    }

    pub fn active_id(&self) -> Option<String> {
        lock(&self.shared.state)
            .active
            .as_ref()
            .map(|(_, id)| id.clone())
    }

    /// A finished load is waiting for the current turn to end.
    pub fn has_pending(&self) -> bool {
        lock(&self.shared.state).pending.is_some()
    }

    fn notify(&self, n: SwapNotice) {
        (self.shared.observer)(n);
    }

    /// A turn starts: mark the slot busy (no swap is applied until [`Self::end_turn`]) and hand
    /// out the active engine. The caller keeps the `Arc` for the whole turn.
    pub fn begin_turn(&self) -> Option<Arc<T>> {
        let mut st = lock(&self.shared.state);
        st.busy = true;
        st.active.as_ref().map(|(e, _)| e.clone())
    }

    /// The turn is over (idle, cancelled or failed): apply a waiting replacement. Idempotent.
    pub fn end_turn(&self) {
        let applied = {
            let mut st = lock(&self.shared.state);
            st.busy = false;
            match st.pending.take() {
                Some((e, id)) => {
                    st.active = Some((e, id.clone()));
                    Some(id)
                }
                None => None,
            }
        };
        if let Some(id) = applied {
            self.notify(SwapNotice::Done { id });
        }
    }

    /// Load a replacement named `id` in the background. Returns at once; the observer hears
    /// `Requested`, then `Done` (when applied) or `Failed`. Asking for the engine that is
    /// already active only drops older requests.
    pub fn request_swap(&self, id: impl Into<String>, loader: Loader<T>) {
        let id = id.into();
        let (generation, already_active) = {
            let mut st = lock(&self.shared.state);
            st.generation += 1;
            st.pending = None; // the newest request wins
            (
                st.generation,
                st.active.as_ref().is_some_and(|(_, a)| *a == id),
            )
        };
        self.notify(SwapNotice::Requested { id: id.clone() });
        if already_active {
            self.notify(SwapNotice::Done { id });
            return;
        }
        let this = self.clone();
        let id2 = id.clone();
        let spawned = std::thread::Builder::new()
            .name("engine-swap".into())
            .spawn(move || {
                let res = catch_unwind(AssertUnwindSafe(loader))
                    .unwrap_or_else(|_| Err(SwapFailure::new(TOKEN_LOAD_FAILED)));
                this.complete(generation, id2, res);
            });
        if spawned.is_err() {
            self.complete(generation, id, Err(SwapFailure::new(TOKEN_LOAD_FAILED)));
        }
    }

    fn complete(&self, generation: u64, id: String, res: Result<Arc<T>, SwapFailure>) {
        let notice = {
            let mut st = lock(&self.shared.state);
            if st.generation != generation {
                return; // superseded by a newer request: drop this result
            }
            match res {
                Ok(engine) => {
                    if st.busy {
                        st.pending = Some((engine, id));
                        return;
                    }
                    st.active = Some((engine, id.clone()));
                    SwapNotice::Done { id }
                }
                Err(failure) => SwapNotice::Failed { id, failure },
            }
        };
        self.notify(notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    trait Eng: Send + Sync {
        fn name(&self) -> String;
    }
    struct E(&'static str);
    impl Eng for E {
        fn name(&self) -> String {
            self.0.into()
        }
    }

    type Log = Arc<Mutex<Vec<SwapNotice>>>;

    fn slot() -> (EngineSlot<dyn Eng>, Log, mpsc::Receiver<()>) {
        let log: Log = Arc::default();
        let (tx, rx) = mpsc::channel();
        let l = log.clone();
        let tx = Mutex::new(tx);
        let s = EngineSlot::new(Arc::new(move |n| {
            let terminal = !matches!(n, SwapNotice::Requested { .. });
            l.lock().unwrap().push(n);
            if terminal {
                let _ = tx.lock().unwrap().send(());
            }
        }));
        (s, log, rx)
    }

    fn ok(name: &'static str) -> Loader<dyn Eng> {
        Box::new(move || Ok(Arc::new(E(name)) as Arc<dyn Eng>))
    }

    fn wait(rx: &mpsc::Receiver<()>) {
        rx.recv_timeout(Duration::from_secs(5)).expect("event");
    }

    fn name(s: &EngineSlot<dyn Eng>) -> Option<String> {
        s.get().map(|e| e.name())
    }

    #[test]
    fn swap_replaces_the_engine_after_the_load() {
        let (s, log, rx) = slot();
        s.set(Arc::new(E("old")), "old");
        s.request_swap("new", ok("new"));
        wait(&rx);
        assert_eq!(name(&s).as_deref(), Some("new"));
        assert_eq!(s.active_id().as_deref(), Some("new"));
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                SwapNotice::Requested { id: "new".into() },
                SwapNotice::Done { id: "new".into() }
            ]
        );
    }

    #[test]
    fn failed_load_keeps_the_old_engine() {
        let (s, log, rx) = slot();
        s.set(Arc::new(E("old")), "old");
        s.request_swap(
            "new",
            Box::new(|| Err(SwapFailure::missing("/models/new.bin"))),
        );
        wait(&rx);
        assert_eq!(name(&s).as_deref(), Some("old"));
        assert_eq!(
            log.lock().unwrap()[1],
            SwapNotice::Failed {
                id: "new".into(),
                failure: SwapFailure::missing("/models/new.bin")
            }
        );
    }

    #[test]
    fn empty_slot_stays_empty_after_a_failed_or_panicking_load() {
        let (s, log, rx) = slot();
        s.request_swap("a", Box::new(|| Err(SwapFailure::new(TOKEN_LOAD_FAILED))));
        wait(&rx);
        assert!(s.get().is_none() && s.begin_turn().is_none());
        s.end_turn();
        s.request_swap("b", Box::new(|| panic!("boom")));
        wait(&rx);
        assert!(s.get().is_none());
        assert!(matches!(
            &log.lock().unwrap()[3],
            SwapNotice::Failed { failure, .. } if failure.token == TOKEN_LOAD_FAILED
        ));
    }

    #[test]
    fn loader_runs_on_its_own_thread() {
        let (s, _log, rx) = slot();
        let (tx, got) = mpsc::channel();
        s.request_swap(
            "x",
            Box::new(move || {
                tx.send(std::thread::current().id()).unwrap();
                Ok(Arc::new(E("x")) as Arc<dyn Eng>)
            }),
        );
        wait(&rx);
        assert_ne!(got.recv().unwrap(), std::thread::current().id());
    }

    #[test]
    fn swap_during_a_turn_waits_and_the_turn_keeps_its_engine() {
        let (s, log, rx) = slot();
        s.set(Arc::new(E("old")), "old");
        let held = s.begin_turn().unwrap();
        s.request_swap("new", ok("new"));
        // The load finishes but is not applied.
        for _ in 0..200 {
            if s.has_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(s.has_pending());
        assert_eq!(name(&s).as_deref(), Some("old"));
        assert_eq!(held.name(), "old");
        assert_eq!(log.lock().unwrap().len(), 1, "no Done yet");
        s.end_turn();
        wait(&rx);
        assert_eq!(name(&s).as_deref(), Some("new"));
        assert_eq!(held.name(), "old", "the running turn is untouched");
        s.end_turn(); // idempotent
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn newest_request_wins_over_a_pending_and_an_inflight_one() {
        let (s, log, rx) = slot();
        s.set(Arc::new(E("old")), "old");
        s.begin_turn();
        s.request_swap("a", ok("a"));
        for _ in 0..200 {
            if s.has_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // "b" is slow: "c" arrives while it is still loading, then "b" finishes late.
        let (release, gate) = mpsc::channel::<()>();
        let (started_tx, started) = mpsc::channel::<()>();
        s.request_swap(
            "b",
            Box::new(move || {
                started_tx.send(()).unwrap();
                gate.recv().unwrap();
                Ok(Arc::new(E("b")) as Arc<dyn Eng>)
            }),
        );
        assert!(!s.has_pending(), "older pending dropped");
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        s.request_swap("c", ok("c"));
        for _ in 0..200 {
            if s.has_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        s.end_turn();
        wait(&rx);
        assert_eq!(name(&s).as_deref(), Some("c"));
        let dones: Vec<_> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|n| matches!(n, SwapNotice::Done { .. }))
            .cloned()
            .collect();
        assert_eq!(dones, vec![SwapNotice::Done { id: "c".into() }]);
    }

    #[test]
    fn requesting_the_active_engine_is_a_no_op_that_still_reports_done() {
        let (s, log, rx) = slot();
        s.set(Arc::new(E("old")), "old");
        s.request_swap("old", Box::new(|| panic!("must not load")));
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                SwapNotice::Requested { id: "old".into() },
                SwapNotice::Done { id: "old".into() }
            ]
        );
        drop(rx);
    }
}
