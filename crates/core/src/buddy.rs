//! `Buddy`: the app-level state the Tauri glue talks to (ADR 0001: the app depends only on
//! this crate): settings, displays, hotkey state, consent and the turn engine (T12).
//!
//! Events go out through the installed [`EventSink`] (default: dropped). The hotkey starts a
//! turn on the engine's runtime ([`crate::turn`]); a press during a running turn cancels it
//! and starts a new one. Push-to-talk only: the microphone is opened after a press and closed
//! on release or cancel; nothing here opens it otherwise. Without an engine (voice engines not
//! built) a press reports `voice_not_built` and does nothing else.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use capture::CaptureSource;
use llm::{
    CloudOptIn, CredentialStore, LlmProvider, LlmRequest, Locality, PreparedRequest,
    RequestPreview, ANTHROPIC_ACCOUNT, ANTHROPIC_PROVIDER_ID,
};
use secrecy::SecretString;
use voice::registry::{Device, Registry, Task};
use voice::{SttProvider, TtsProvider};

use crate::api::{
    Decision, DisplayDto, DownloadDone, DownloadFailed, DownloadProgress, EngineSlotName,
    EngineSwapEvent, FirewallStatus, LastRequestDto, ModelRow, Phase, PointEvent, Settings,
    SettingsPatch, Status, SwapTarget, TurnState,
};
use crate::consent::{
    last_request_dto, provider_allowed, ConsentError, ConsentGate, PreviewSink, PreviewStore,
    LAST_KEY,
};
use crate::download::{DownloadError, DownloadNotice, DownloadObserver, Downloader, ModelManager};
use crate::engine_slot::{
    EngineLoader, EngineSlot, SwapFailure, SwapNotice, SwapObserver, TOKEN_MODEL_MISSING,
};
use crate::error::{simple_turn_error, CoreError, KeyError};
use crate::events::{EventSink, NullSink};
use crate::point::sanitize_label;
use crate::settings::{builtin_registry, SettingsError, SettingsStore};
use crate::turn::{self, EngineDeps, ProviderSource, TurnControl, TurnCtx};

/// Result of a settings update; the glue re-registers the hotkey when `hotkey_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsUpdate {
    pub settings: Settings,
    /// `(old, new)` canonical shortcut strings when the hotkey changed.
    pub hotkey_changed: Option<(String, String)>,
}

#[derive(Debug)]
struct Inner {
    turn_id: u32,
    phase: Phase,
    firewall: FirewallStatus,
    hotkey_registered: bool,
}

/// Why `request_engine_swap` refused. Fixed tokens (the `Display` text), no content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SwapError {
    /// No loader is installed (voice engines not built).
    #[error("loader_unavailable")]
    NoLoader,
    /// The profile or one of its models is not valid in the registry.
    #[error("invalid_model")]
    InvalidModel,
}

/// The `(stt, tts)` model ids of `profile`, validated against `reg`: the profile and models
/// must exist with the right task, and STT never runs on the GPU.
pub fn engine_models(reg: &Registry, profile: &str) -> Result<(String, String), SwapError> {
    let p = reg.profile(profile).ok_or(SwapError::InvalidModel)?;
    let ok = |id: &str, task: Task| reg.get(id).is_ok_and(|m| m.task == task);
    if !ok(&p.stt.model, Task::Stt) || !ok(&p.tts.model, Task::Tts) || p.stt.device == Device::Gpu {
        return Err(SwapError::InvalidModel);
    }
    Ok((p.stt.model.clone(), p.tts.model.clone()))
}

fn failure_notice(slot: EngineSlotName, id: &str, f: &SwapFailure) -> String {
    let which = match slot {
        EngineSlotName::Stt => "speech recognition",
        EngineSlotName::Tts => "speech synthesis",
    };
    match (&f.path, f.token) {
        (Some(path), TOKEN_MODEL_MISSING) => format!(
            "Missing model file for {id}: place it at {path} (nothing is downloaded automatically)."
        ),
        _ => format!("Could not load the {which} model {id} ({}).", f.token),
    }
}

/// Longest accepted API key (bytes).
const MAX_KEY_LEN: usize = 512;

pub struct Buddy {
    settings: Mutex<SettingsStore>,
    inner: Mutex<Inner>,
    pub(crate) capture: Arc<dyn CaptureSource>,
    store: Arc<PreviewStore>,
    pub(crate) consent: Arc<ConsentGate>,
    credentials: Arc<dyn CredentialStore>,
    /// Cached "a key is stored" (best effort); refreshed after set/clear.
    has_key: Mutex<Option<bool>>,
    /// Most recent turn's request (in memory only, replaced each turn).
    last: Mutex<Option<LastRequestDto>>,
    sink: Arc<Mutex<Arc<dyn EventSink>>>,
    /// The swappable voice engines (S2-2). Turns take their engines from here at their start.
    stt: EngineSlot<dyn SttProvider>,
    tts: EngineSlot<dyn TtsProvider>,
    loader: Mutex<Option<Arc<dyn EngineLoader>>>,
    /// Model downloads (S2-4); `None` until the shell calls `set_models_dir`.
    models: Mutex<Option<ModelManager>>,
    /// Latest failed swap per slot, shown as a status notice until the next request.
    swap_failures: Arc<Mutex<BTreeMap<EngineSlotName, String>>>,
    /// The turn engine's dependencies; `None` = voice engines not built.
    engine: Mutex<Option<Arc<EngineDeps>>>,
    current: Mutex<Option<TurnControl>>,
    notices: Mutex<Vec<String>>,
    /// Why there is no engine (reported on a hotkey press): `(code, message)`.
    engine_unavailable: Mutex<Option<(String, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Buddy {
    pub fn new(settings_path: impl Into<PathBuf>, capture: Box<dyn CaptureSource>) -> Self {
        Self::with_credentials(settings_path, capture, llm::default_credential_store())
    }

    pub fn with_credentials(
        settings_path: impl Into<PathBuf>,
        capture: Box<dyn CaptureSource>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        let settings = SettingsStore::load(settings_path);
        let store = Arc::new(PreviewStore::new());
        let consent = ConsentGate::new(store.clone());
        consent.set_local_only(settings.settings().local_only);
        let sink: Arc<Mutex<Arc<dyn EventSink>>> = Arc::new(Mutex::new(Arc::new(NullSink)));
        let swap_failures: Arc<Mutex<BTreeMap<EngineSlotName, String>>> = Arc::default();
        let observer = |slot: EngineSlotName| -> SwapObserver {
            let (sink, failures) = (sink.clone(), swap_failures.clone());
            Arc::new(move |n: SwapNotice| {
                let ev = |id: String, f: Option<&SwapFailure>| EngineSwapEvent {
                    slot,
                    engine_id: id,
                    error: f.map(|f| f.token.to_string()),
                    path: f.and_then(|f| f.path.clone()),
                };
                let out = lock(&sink).clone();
                match n {
                    SwapNotice::Requested { id } => {
                        lock(&failures).remove(&slot);
                        out.engine_swap_requested(ev(id, None));
                    }
                    SwapNotice::Done { id } => {
                        lock(&failures).remove(&slot);
                        out.engine_swap_done(ev(id, None));
                    }
                    SwapNotice::Failed { id, failure } => {
                        lock(&failures).insert(slot, failure_notice(slot, &id, &failure));
                        out.engine_swap_failed(ev(id, Some(&failure)));
                    }
                }
            })
        };
        let (stt, tts) = (
            EngineSlot::new(observer(EngineSlotName::Stt)),
            EngineSlot::new(observer(EngineSlotName::Tts)),
        );
        Self {
            settings: Mutex::new(settings),
            inner: Mutex::new(Inner {
                turn_id: 0,
                phase: Phase::Idle,
                firewall: FirewallStatus::default(),
                hotkey_registered: false,
            }),
            capture: Arc::from(capture),
            store,
            consent,
            credentials,
            has_key: Mutex::new(None),
            last: Mutex::new(None),
            sink,
            stt,
            tts,
            loader: Mutex::new(None),
            models: Mutex::new(None),
            swap_failures,
            engine: Mutex::new(None),
            current: Mutex::new(None),
            notices: Mutex::new(Vec::new()),
            engine_unavailable: Mutex::new(None),
        }
    }

    /// Where events go (the glue installs this once at startup).
    pub fn set_event_sink(&self, sink: Arc<dyn EventSink>) {
        *lock(&self.sink) = sink;
    }

    pub(crate) fn sink(&self) -> Arc<dyn EventSink> {
        lock(&self.sink).clone()
    }

    /// The STT engine slot (tests and fixed setups may `set` an engine directly).
    pub fn stt_slot(&self) -> &EngineSlot<dyn SttProvider> {
        &self.stt
    }

    /// The TTS engine slot.
    pub fn tts_slot(&self) -> &EngineSlot<dyn TtsProvider> {
        &self.tts
    }

    /// Install what builds STT/TTS engines for `request_engine_swap` (the real adapters, or a
    /// fake in tests).
    pub fn set_engine_loader(&self, loader: Arc<dyn EngineLoader>) {
        *lock(&self.loader) = Some(loader);
    }

    /// Enable the models screen for `dir` (the shell calls this once at startup). Touches
    /// nothing: no directory is created and nothing is downloaded until the user clicks.
    pub fn set_models_dir(&self, dir: PathBuf) {
        self.set_model_manager(dir, Downloader::new(), builtin_registry().clone());
    }

    pub(crate) fn set_model_manager(&self, dir: PathBuf, dl: Downloader, reg: Registry) {
        let sink = self.sink.clone();
        // Events go out with no lock held (L-013): the sink is only cloned under its lock.
        let observer: DownloadObserver = Arc::new(move |n: DownloadNotice| {
            let out = lock(&sink).clone();
            match n {
                DownloadNotice::Progress { id, bytes, total } => {
                    out.download_progress(DownloadProgress { id, bytes, total })
                }
                DownloadNotice::Done { id } => out.download_done(DownloadDone { id }),
                DownloadNotice::Failed { id, code } => out.download_failed(DownloadFailed {
                    id,
                    code: code.to_string(),
                }),
            }
        });
        *lock(&self.models) = Some(ModelManager::new(reg, dir, dl, observer));
    }

    fn model_manager(&self) -> Option<ModelManager> {
        lock(&self.models).clone()
    }

    /// `list_models`: the models table from the local filesystem only (no network).
    pub fn list_models(&self) -> Vec<ModelRow> {
        let profile = lock(&self.settings).settings().profile;
        self.model_manager()
            .map(|m| m.list(&profile))
            .unwrap_or_default()
    }

    /// `start_download`: explicit click only. `accept_unknown_checksum` is the second confirm
    /// for registry entries whose checksum is `TODO`. Errors are fixed tokens.
    pub fn start_download(
        &self,
        id: &str,
        accept_unknown_checksum: bool,
    ) -> Result<(), DownloadError> {
        self.model_manager()
            .ok_or(DownloadError::Unavailable)?
            .start(id, accept_unknown_checksum)
    }

    /// `cancel_download`: stops the running download of `id` and deletes its partial files.
    pub fn cancel_download(&self, id: &str) {
        if let Some(m) = self.model_manager() {
            m.cancel(id);
        }
    }

    /// `request_engine_swap` command (design: explicit command, called by the settings window
    /// after a profile change and by the startup code for the first load; `update_settings`
    /// itself never loads anything). The model ids come from the CURRENT profile, validated
    /// against the registry (STT never on the GPU); an invalid profile or model is refused
    /// before anything starts. Loads run in the background, a running push-to-talk turn is
    /// never disturbed, and results arrive as `engine_swap_*` events.
    pub fn request_engine_swap(&self, target: SwapTarget) -> Result<(), SwapError> {
        let loader = lock(&self.loader).clone().ok_or(SwapError::NoLoader)?;
        let profile = lock(&self.settings).settings().profile;
        let (stt_id, tts_id) = engine_models(builtin_registry(), &profile)?;
        if matches!(target, SwapTarget::Stt | SwapTarget::All) {
            let (l, id) = (loader.clone(), stt_id.clone());
            self.stt
                .request_swap(stt_id, Box::new(move || l.load_stt(&id)));
        }
        if matches!(target, SwapTarget::Tts | SwapTarget::All) {
            let (l, id) = (loader, tts_id.clone());
            self.tts
                .request_swap(tts_id, Box::new(move || l.load_tts(&id)));
        }
        Ok(())
    }

    /// Install the turn engine. Until this is called a hotkey press reports `voice_not_built`.
    pub fn attach_engine(&self, deps: EngineDeps) {
        *lock(&self.engine) = Some(Arc::new(deps));
    }

    /// Why no engine is attached, for the `turn:error` a hotkey press reports (`code` and
    /// `message` must be fixed text: no content).
    pub fn set_engine_unavailable(&self, code: &str, message: &str) {
        *lock(&self.engine_unavailable) = Some((code.to_string(), message.to_string()));
    }

    pub fn has_engine(&self) -> bool {
        lock(&self.engine).is_some()
    }

    /// The provider source for the real adapters (Ollama on loopback, Anthropic with the key
    /// from this buddy's credential store).
    pub fn default_providers(&self) -> Arc<dyn ProviderSource> {
        Arc::new(crate::providers::DefaultProviders::new(
            self.credentials.clone(),
        ))
    }

    /// Replace the display-only notes shown in `Status.notices`.
    pub fn set_notices(&self, notices: Vec<String>) {
        *lock(&self.notices) = notices;
    }

    pub fn add_notice(&self, notice: String) {
        let mut n = lock(&self.notices);
        if !n.contains(&notice) {
            n.push(notice);
        }
    }

    fn has_api_key(&self) -> bool {
        let mut c = lock(&self.has_key);
        *c.get_or_insert_with(|| matches!(self.credentials.get(ANTHROPIC_ACCOUNT), Ok(Some(_))))
    }

    /// Fill the "is a key stored" cache (one keychain read). Call it off the async workers at
    /// startup so the first turn's `get_settings` does not block a runtime worker on the keychain.
    pub fn prime_api_key_cache(&self) {
        let _ = self.has_api_key();
    }

    fn refresh_has_api_key(&self) {
        *lock(&self.has_key) = None;
    }

    pub fn get_settings(&self) -> Settings {
        let mut s = lock(&self.settings).settings();
        s.has_api_key = self.has_api_key();
        s
    }

    pub fn update_settings(&self, patch: &SettingsPatch) -> Result<SettingsUpdate, SettingsError> {
        let (mut settings, hotkey_changed) = {
            let mut store = lock(&self.settings);
            let old = store.settings().hotkey;
            let settings = store.apply(patch)?;
            let hotkey_changed = (settings.hotkey != old).then(|| (old, settings.hotkey.clone()));
            (settings, hotkey_changed)
        };
        // Mirror local_only into the consent gate (turning it on cancels pending previews).
        self.consent.set_local_only(settings.local_only);
        settings.has_api_key = self.has_api_key();
        Ok(SettingsUpdate {
            settings,
            hotkey_changed,
        })
    }

    /// Store the Anthropic API key in the OS keychain. The key is never logged, persisted in
    /// settings or returned; errors are fixed tokens.
    pub fn set_api_key(&self, provider: &str, key: String) -> Result<Settings, KeyError> {
        if provider != ANTHROPIC_PROVIDER_ID {
            return Err(KeyError::UnsupportedProvider);
        }
        let key = SecretString::from(key.trim().to_string());
        {
            use secrecy::ExposeSecret;
            let k = key.expose_secret();
            if k.is_empty() || k.len() > MAX_KEY_LEN || k.chars().any(char::is_control) {
                return Err(KeyError::InvalidKey);
            }
        }
        let res = self.credentials.set(ANTHROPIC_ACCOUNT, key);
        self.refresh_has_api_key();
        res?;
        Ok(self.get_settings())
    }

    pub fn clear_api_key(&self, provider: &str) -> Result<Settings, KeyError> {
        if provider != ANTHROPIC_PROVIDER_ID {
            return Err(KeyError::UnsupportedProvider);
        }
        let res = self.credentials.delete(ANTHROPIC_ACCOUNT);
        self.refresh_has_api_key();
        res?;
        Ok(self.get_settings())
    }

    // ------------------------------------------------------------ consent / preview

    /// Where the panel is shown (the glue installs this; the default cannot show anything and
    /// so cancels every cloud request).
    pub fn set_preview_sink(&self, sink: Arc<dyn PreviewSink>) {
        self.consent.set_sink(sink);
    }

    /// In-memory previews for the `preview://` URI scheme.
    pub fn preview_store(&self) -> Arc<PreviewStore> {
        self.store.clone()
    }

    /// `preview_decision` command.
    pub fn preview_decision(
        &self,
        request_id: u64,
        decision: Decision,
    ) -> Result<(), ConsentError> {
        self.consent.decide(request_id, decision)
    }

    /// Cancel every pending preview (window closed, app quitting, turn cancelled).
    pub fn cancel_pending_consents(&self) {
        self.consent.cancel_all();
    }

    /// `get_last_request` command: the most recent turn's request (local or cloud).
    pub fn get_last_request(&self) -> Option<LastRequestDto> {
        lock(&self.last).clone()
    }

    /// `get_pending_preview` command (preview window only): the preview currently waiting for
    /// a decision, so a panel that mounts after `turn:preview` was emitted still shows it.
    pub fn pending_preview(&self) -> Option<crate::api::PreviewDto> {
        self.consent.pending_preview()
    }

    fn record_last(&self, preview: &RequestPreview) {
        let turn_id = lock(&self.inner).turn_id;
        self.store.put(LAST_KEY, preview);
        *lock(&self.last) = Some(last_request_dto(turn_id, preview));
    }

    /// The whole consent policy for one request (T12 wires the streaming around it):
    /// - cloud provider while `local_only`: `Err(LocalOnly)`, nothing is prepared or shown;
    /// - local provider: `(prepared, None)`, no panel;
    /// - cloud provider: show the exact request in the preview panel and wait; `Send` gives
    ///   `(prepared, Some(opt_in))` bound to that very request; Cancel, timeout, a newer turn
    ///   or quit give `Err(Cancelled)`.
    ///
    /// The request is also kept as the "last request" (in memory, replaced each turn).
    pub async fn prepare_and_consent(
        &self,
        provider: &dyn LlmProvider,
        request: &LlmRequest,
    ) -> Result<(PreparedRequest, Option<CloudOptIn>), CoreError> {
        let settings = lock(&self.settings).settings();
        provider_allowed(&settings, provider.locality())?;
        let prepared = provider.prepare(request)?;
        // Debug tripwire (S2-5): panics if a registered title/URL marker is in the built
        // request. Empty in release builds; fed only by tests and fakes.
        #[cfg(any(debug_assertions, test))]
        crate::leakguard::assert_no_known_titles(&prepared);
        // The prepared request, not the provider's claim, decides: either side saying Cloud
        // means cloud.
        let locality =
            if provider.locality() == Locality::Cloud || prepared.locality() == Locality::Cloud {
                Locality::Cloud
            } else {
                Locality::Local
            };
        provider_allowed(&settings, locality)?;
        self.record_last(&prepared.preview());
        if locality == Locality::Local {
            return Ok((prepared, None));
        }
        let turn_id = lock(&self.inner).turn_id;
        let pending = self.consent.request_consent(&prepared, turn_id)?;
        let opt_in = pending.wait().await?;
        Ok((prepared, Some(opt_in)))
    }

    pub fn get_status(&self) -> Status {
        let settings_error = lock(&self.settings).load_error().map(String::from);
        let i = lock(&self.inner);
        Status {
            phase: i.phase,
            firewall: i.firewall.clone(),
            hotkey_registered: i.hotkey_registered,
            settings_error,
            notices: {
                let mut n = lock(&self.notices).clone();
                n.extend(lock(&self.swap_failures).values().cloned());
                n
            },
        }
    }

    /// The glue reports whether the global shortcut could be registered.
    pub fn set_hotkey_registered(&self, ok: bool) {
        lock(&self.inner).hotkey_registered = ok;
    }

    /// Displays in the capture crate's coordinates. Empty when the platform cannot list them
    /// (Linux dev machines): no overlay is created then (fail closed).
    pub fn list_displays(&self) -> Vec<DisplayDto> {
        self.capture
            .displays()
            .map(|d| d.iter().map(DisplayDto::from).collect())
            .unwrap_or_default()
    }

    /// Push-to-talk pressed.
    ///
    /// - while listening (key auto-repeat): ignored;
    /// - during a running turn (transcribing, thinking, speaking): that turn is cancelled
    ///   (streams dropped, playback stopped, pending consent cancelled) and a new one starts;
    /// - otherwise a new turn starts: `turn:state Listening`, mic opened.
    pub fn on_hotkey_pressed(self: &Arc<Self>) {
        let Some(deps) = lock(&self.engine).clone() else {
            let (code, msg) = lock(&self.engine_unavailable).clone().unwrap_or_else(|| {
                (
                    "voice_not_built".into(),
                    "Voice engines are not available. See the status notices.".into(),
                )
            });
            self.sink().error(simple_turn_error(&code, &msg));
            return;
        };
        let sink = self.sink();
        {
            let i = lock(&self.inner);
            if i.phase == Phase::Listening {
                return;
            }
        }
        if let Some(old) = self.cancel_current() {
            sink.turn_state(old);
        }
        // The cancelled turn is over: a waiting engine swap lands now, before the new turn
        // takes its engines.
        self.engines_idle();
        let (Some(stt), Some(tts)) = (self.stt.begin_turn(), self.tts.begin_turn()) else {
            self.engines_idle();
            let (code, msg) = (
                "engine_not_loaded",
                "The voice engines are not loaded yet. See the status notices.",
            );
            sink.error(simple_turn_error(code, msg));
            return;
        };
        // A new turn supersedes any preview still waiting from an earlier one.
        self.consent.cancel_all();
        let ev = {
            let mut i = lock(&self.inner);
            i.turn_id = i.turn_id.wrapping_add(1);
            i.phase = Phase::Listening;
            let turn_id = i.turn_id;
            let cancelled = Arc::new(AtomicBool::new(false));
            let (release_tx, release_rx) = tokio::sync::watch::channel(false);
            let ctx = TurnCtx {
                buddy: self.clone(),
                deps: deps.clone(),
                turn_id,
                cancelled: cancelled.clone(),
                release: release_rx,
                stt,
                tts,
            };
            let task = deps.runtime.spawn(turn::run(ctx));
            *lock(&self.current) = Some(TurnControl {
                turn_id,
                cancelled,
                release: release_tx,
                task,
            });
            TurnState {
                turn_id,
                phase: Phase::Listening,
            }
        };
        sink.turn_state(ev);
    }

    /// Push-to-talk released: the mic is closed and the turn moves to `Transcribing`.
    pub fn on_hotkey_released(&self) {
        let ev = {
            let mut i = lock(&self.inner);
            if i.phase != Phase::Listening {
                return;
            }
            i.phase = Phase::Transcribing;
            TurnState {
                turn_id: i.turn_id,
                phase: i.phase,
            }
        };
        if let Some(c) = lock(&self.current).as_ref() {
            let _ = c.release.send(true);
        }
        self.sink().turn_state(ev);
    }

    /// `cancel_turn` command: stop the running turn (if any) and cancel pending previews.
    pub fn cancel_turn(&self) {
        let ev = self.cancel_current();
        self.consent.cancel_all();
        if let Some(ev) = ev {
            self.sink().turn_state(ev);
        }
        self.engines_idle();
    }

    /// No turn is running any more: let waiting engine swaps land.
    fn engines_idle(&self) {
        self.stt.end_turn();
        self.tts.end_turn();
    }

    /// Abort the running turn. Returns the `Idle` state to emit when a turn was in progress.
    fn cancel_current(&self) -> Option<TurnState> {
        let mut i = lock(&self.inner);
        if let Some(c) = lock(&self.current).take() {
            c.cancel();
        }
        if i.phase == Phase::Idle {
            return None;
        }
        i.phase = Phase::Idle;
        Some(TurnState {
            turn_id: i.turn_id,
            phase: Phase::Idle,
        })
    }

    /// Engine -> state: move the running turn to `phase` (ignored for a stale turn).
    pub(crate) fn advance_phase(&self, turn_id: u32, phase: Phase) {
        let ev = {
            let mut i = lock(&self.inner);
            if i.turn_id != turn_id
                || i.phase == phase
                || i.phase == Phase::Idle
                // Never step back from Speaking (a spoken explanation can start before the
                // model is called).
                || (phase == Phase::Thinking && i.phase == Phase::Speaking)
            {
                return;
            }
            i.phase = phase;
            TurnState { turn_id, phase }
        };
        self.sink().turn_state(ev);
    }

    /// Engine -> state: the turn is over.
    pub(crate) fn finish_turn(&self, turn_id: u32) {
        if self.finish_turn_state(turn_id) {
            // The turn is over: a waiting engine swap lands now (after the Idle event).
            self.engines_idle();
        }
    }

    /// Returns false for a stale turn (a newer one owns the engines now).
    fn finish_turn_state(&self, turn_id: u32) -> bool {
        let ev = {
            let mut i = lock(&self.inner);
            if i.turn_id != turn_id {
                return false;
            }
            {
                let mut cur = lock(&self.current);
                if cur.as_ref().is_some_and(|c| c.turn_id == turn_id) {
                    *cur = None;
                }
            }
            if i.phase == Phase::Idle {
                return true;
            }
            i.phase = Phase::Idle;
            TurnState {
                turn_id,
                phase: Phase::Idle,
            }
        };
        self.sink().turn_state(ev);
        true
    }

    /// Engine -> state: remember and announce the firewall's state for this turn.
    pub(crate) fn set_firewall_status(&self, st: FirewallStatus) {
        lock(&self.inner).firewall = st.clone();
        self.sink().firewall_status(st);
    }

    /// Dev-only point for manual overlay tests: `(x, y)` in global points; the display is the
    /// one containing the point. `None` when no display contains it.
    pub fn debug_point(&self, x: i32, y: i32, label: &str) -> Option<PointEvent> {
        let displays = self.capture.displays().ok()?;
        let d = displays.iter().find(|d| {
            let b = d.bounds();
            (x as i64) >= b.x as i64
                && (y as i64) >= b.y as i64
                && (x as i64) < b.x as i64 + b.w as i64
                && (y as i64) < b.y as i64 + b.h as i64
        })?;
        Some(PointEvent {
            x,
            y,
            label: sanitize_label(label),
            display_id: d.id.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture::fake::{FakeCapture, FakeMode};
    use frame::{DisplayId, DisplayInfo};

    fn buddy() -> (tempfile::TempDir, Buddy) {
        let d = tempfile::tempdir().unwrap();
        let mut fake = FakeCapture::new(FakeMode::ExcludeWindows);
        fake.display = DisplayInfo {
            id: DisplayId(5),
            origin: (-100, 0),
            size: (200, 100),
            scale: 1.0,
        };
        let b = Buddy::new(d.path().join("s.json"), Box::new(fake));
        (d, b)
    }

    #[test]
    fn hotkey_without_an_engine_reports_voice_not_built_and_stays_idle() {
        #[derive(Default)]
        struct Rec(Mutex<Vec<String>>);
        impl EventSink for Rec {
            fn turn_state(&self, e: TurnState) {
                lock(&self.0).push(format!("state:{:?}", e.phase));
            }
            fn transcript(&self, _: crate::api::TranscriptEvent) {}
            fn reply_delta(&self, _: crate::api::ReplyDelta) {}
            fn point(&self, _: PointEvent) {}
            fn firewall_status(&self, _: FirewallStatus) {}
            fn error(&self, e: crate::api::TurnError) {
                lock(&self.0).push(format!("error:{}", e.code));
            }
        }
        let (_d, b) = buddy();
        let b = Arc::new(b);
        let rec = Arc::new(Rec::default());
        b.set_event_sink(rec.clone());
        assert!(!b.has_engine());
        b.on_hotkey_pressed();
        b.on_hotkey_released();
        assert_eq!(b.get_status().phase, Phase::Idle);
        assert_eq!(*lock(&rec.0), vec!["error:voice_not_built".to_string()]);
    }

    #[test]
    fn settings_update_reports_hotkey_change() {
        let (_d, b) = buddy();
        let u = b
            .update_settings(&SettingsPatch {
                hotkey: Some("alt+k".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            u.hotkey_changed,
            Some(("Ctrl+Alt+Space".into(), "Alt+K".into()))
        );
        let u = b
            .update_settings(&SettingsPatch {
                local_only: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert!(u.hotkey_changed.is_none());
        assert!(!b.get_settings().local_only);
        assert!(b.get_status().settings_error.is_none());
    }

    #[test]
    fn displays_and_debug_point() {
        let (_d, b) = buddy();
        let ds = b.list_displays();
        assert_eq!(ds.len(), 1);
        assert_eq!((ds[0].id, ds[0].x, ds[0].w), (5, -100, 200));
        let p = b.debug_point(-50, 10, "hi\u{7}").unwrap();
        assert_eq!(
            (p.x, p.y, p.display_id, p.label.as_str()),
            (-50, 10, 5, "hi")
        );
        assert!(b.debug_point(100, 10, "x").is_none());
        assert!(b.debug_point(-101, 10, "x").is_none());
    }

    #[test]
    fn api_key_ipc_is_allowlisted_masked_and_never_persisted() {
        use llm::fakes::FakeCredentialStore;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.json");
        let creds = Arc::new(FakeCredentialStore::new());
        let b =
            Buddy::with_credentials(&path, Box::new(capture::UnsupportedCapture), creds.clone());
        assert!(!b.get_settings().has_api_key);
        assert_eq!(
            b.set_api_key("openai", "sk-x".into()).unwrap_err(),
            KeyError::UnsupportedProvider
        );
        assert_eq!(
            b.clear_api_key("openai").unwrap_err(),
            KeyError::UnsupportedProvider
        );
        for bad in ["", "   ", "a\nb", &"k".repeat(513)] {
            assert_eq!(
                b.set_api_key("anthropic", bad.into()).unwrap_err(),
                KeyError::InvalidKey
            );
        }
        let s = b
            .set_api_key("anthropic", "  SECRET-KEY-VALUE\n".into())
            .unwrap();
        assert!(s.has_api_key);
        assert!(b.get_settings().has_api_key);
        // Stored trimmed, in the store only: not in settings.json, not in Settings/Debug.
        use secrecy::ExposeSecret;
        assert_eq!(
            creds
                .get(ANTHROPIC_ACCOUNT)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "SECRET-KEY-VALUE"
        );
        b.update_settings(&SettingsPatch {
            local_only: Some(false),
            ..Default::default()
        })
        .unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(!file.contains("SECRET-KEY-VALUE"));
        assert!(!format!("{:?}", b.get_settings()).contains("SECRET-KEY-VALUE"));
        let s = b.clear_api_key("anthropic").unwrap();
        assert!(!s.has_api_key);
        assert!(!b.get_settings().has_api_key);
    }

    #[test]
    fn unsupported_credential_store_gives_typed_error() {
        let d = tempfile::tempdir().unwrap();
        let b = Buddy::with_credentials(
            d.path().join("s.json"),
            Box::new(capture::UnsupportedCapture),
            Arc::new(llm::UnsupportedCredentialStore),
        );
        let e = b.set_api_key("anthropic", "k".into()).unwrap_err();
        assert_eq!(e, KeyError::StoreUnsupported);
        assert_eq!(e.to_string(), "credential_store_unsupported");
        assert_eq!(
            b.clear_api_key("anthropic").unwrap_err().to_string(),
            "credential_store_unsupported"
        );
        assert!(!b.get_settings().has_api_key);
    }

    #[test]
    fn unsupported_capture_lists_no_displays() {
        let d = tempfile::tempdir().unwrap();
        let b = Buddy::new(
            d.path().join("s.json"),
            Box::new(capture::UnsupportedCapture),
        );
        assert!(b.list_displays().is_empty());
        assert!(b.debug_point(0, 0, "x").is_none());
    }
}
