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
use std::sync::atomic::{AtomicBool, Ordering};
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
    AuditClearDto, AuditExportDto, AuditPageDto, Decision, DenyListDto, DisplayDto, DownloadDone,
    DownloadFailed, DownloadProgress, EngineSlotName, EngineSwapEvent, FirewallStatus,
    LastRequestDto, ModelRow, Phase, PointEvent, Settings, SettingsPatch, SettingsResult, Status,
    SwapTarget, TurnState,
};
use crate::audit_log::{page_dto, validate_export_path, AuditCmdError, DEFAULT_PAGE};
use crate::consent::{
    last_request_dto, provider_allowed, ConsentError, ConsentGate, PreviewSink, PreviewStore,
    LAST_KEY,
};
use crate::deny_edit::{DenyEditError, DenyEditor};
use crate::download::{
    DownloadError, DownloadNotice, DownloadObserver, Downloader, ModelManager, SharedDir,
};
use crate::engine_slot::{
    EngineLoader, EngineSlot, SwapFailure, SwapNotice, SwapObserver, TOKEN_MODEL_MISSING,
};
use crate::error::{simple_turn_error, CoreError, KeyError};
use crate::events::{EventSink, NullSink};
use crate::point::sanitize_label;
use crate::settings::{builtin_registry, SettingsError, SettingsStore};
use crate::setup::BANKING_NOTICE;
use crate::turn::{self, EngineDeps, ProviderSource, TurnControl, TurnCtx};

/// Result of a settings update; the glue re-registers the hotkey when `hotkey_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsUpdate {
    pub settings: Settings,
    /// `(old, new)` canonical shortcut strings when the hotkey changed.
    pub hotkey_changed: Option<(String, String)>,
    /// Values that were not accepted and fell back to the safest value.
    pub issues: Vec<crate::api::SettingsIssue>,
    /// The effective models directory changed (refresh the table, request an engine swap).
    pub models_dir_changed: bool,
}

impl SettingsUpdate {
    /// The IPC shape of the update.
    pub fn result(&self) -> SettingsResult {
        SettingsResult {
            settings: self.settings.clone(),
            issues: self.issues.clone(),
            models_dir_changed: self.models_dir_changed,
        }
    }
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

/// Shown when the activity log could not be set up or written.
const AUDIT_NOTICE: &str =
    "The activity log is not working, so the agent stays off until it can be written.";

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
    /// The built-in models directory (set by the shell) and the one in use (the setting, or
    /// the built-in one). The shared cell is what the engine loaders read.
    default_models_dir: Mutex<Option<PathBuf>>,
    models_cell: SharedDir,
    /// The deny-list editor; `None` until the shell calls `set_config_dir` (then the engine's
    /// own list is used unchanged).
    deny_editor: Mutex<Option<DenyEditor>>,
    /// Latest failed swap per slot, shown as a status notice until the next request.
    swap_failures: Arc<Mutex<BTreeMap<EngineSlotName, String>>>,
    /// The turn engine's dependencies; `None` = voice engines not built.
    engine: Mutex<Option<Arc<EngineDeps>>>,
    current: Mutex<Option<TurnControl>>,
    notices: Mutex<Vec<String>>,
    /// Why there is no engine (reported on a hotkey press): `(code, message)`.
    engine_unavailable: Mutex<Option<(String, String)>>,
    /// The append-only agent activity log (S2-12). The future gate is built with this sink; until
    /// `set_data_dir` succeeds every `intent` fails (fail closed: no audit, no agent).
    audit: Arc<audit::FileAuditSink>,
    audit_attempted: AtomicBool,
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
            default_models_dir: Mutex::new(None),
            models_cell: Arc::default(),
            deny_editor: Mutex::new(None),
            swap_failures,
            engine: Mutex::new(None),
            current: Mutex::new(None),
            notices: Mutex::new(Vec::new()),
            engine_unavailable: Mutex::new(None),
            audit: Arc::new(audit::FileAuditSink::unattached()),
            audit_attempted: AtomicBool::new(false),
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
    ///
    /// `dir` is the built-in default; the `models_dir` setting, when set, wins.
    pub fn set_models_dir(&self, dir: PathBuf) {
        *lock(&self.default_models_dir) = Some(dir);
        if let Some(dir) = self.models_dir() {
            self.set_model_manager(dir, Downloader::new(), builtin_registry().clone());
        }
    }

    /// The models directory in use: the setting if set, else the built-in default. `None`
    /// until the shell has set the default.
    pub fn models_dir(&self) -> Option<PathBuf> {
        let custom = lock(&self.settings).settings().models_dir;
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
        lock(&self.default_models_dir).clone()
    }

    /// The models directory as a shared cell for the engine loaders (follows the setting).
    pub fn models_dir_cell(&self) -> SharedDir {
        self.models_cell.clone()
    }

    pub(crate) fn set_model_manager(&self, dir: PathBuf, dl: Downloader, reg: Registry) {
        *lock(&self.models_cell) = dir.clone();
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
        s.default_models_dir = self.default_models_dir_text();
        s
    }

    fn default_models_dir_text(&self) -> String {
        lock(&self.default_models_dir)
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Validate and store a settings patch. Invalid values never fail the call: each falls
    /// back to the safest value and comes back as an issue (the rejected text is not kept).
    /// Only a failed write is an error.
    pub fn update_settings(&self, patch: &SettingsPatch) -> Result<SettingsUpdate, SettingsError> {
        let old_dir = self.models_dir();
        let (mut settings, hotkey_changed, issues) = {
            let mut store = lock(&self.settings);
            let old = store.settings().hotkey;
            let (settings, issues) = store.apply_with_issues(patch)?;
            let hotkey_changed = (settings.hotkey != old).then(|| (old, settings.hotkey.clone()));
            (settings, hotkey_changed, issues)
        };
        // Mirror local_only into the consent gate (turning it on cancels pending previews).
        // Turning it OFF changes nothing else: every cloud request still needs its preview.
        self.consent.set_local_only(settings.local_only);
        settings.has_api_key = self.has_api_key();
        settings.default_models_dir = self.default_models_dir_text();
        let new_dir = self.models_dir();
        let models_dir_changed = old_dir != new_dir;
        if models_dir_changed {
            self.switch_models_dir(new_dir);
        }
        Ok(SettingsUpdate {
            settings,
            hotkey_changed,
            issues,
            models_dir_changed,
        })
    }

    /// The models directory changed: stop a running download (it was writing to the old
    /// folder) and point the models table and the engine loaders at the new one.
    fn switch_models_dir(&self, dir: Option<PathBuf>) {
        let Some(dir) = dir else { return };
        if let Some(old) = self.model_manager() {
            let profile = lock(&self.settings).settings().profile;
            for row in old.list(&profile) {
                if row.status == crate::api::ModelStatus::Downloading {
                    old.cancel(&row.id);
                }
            }
        }
        // Keep the engines' view in step even before the models screen is enabled.
        *lock(&self.models_cell) = dir.clone();
        if lock(&self.models).is_some() {
            self.set_model_manager(dir, Downloader::new(), builtin_registry().clone());
        }
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

    // ------------------------------------------------------------ deny-list editor

    /// Tell the core where `deny_list.json` lives (the shell calls this once at startup).
    /// Loads the list; a broken file falls back to the built-in list and is reported in the
    /// status notices.
    pub fn set_config_dir(&self, dir: PathBuf) {
        *lock(&self.deny_editor) = Some(DenyEditor::load(dir));
    }

    /// The deny list turns must use (`None` before `set_config_dir`).
    pub fn current_deny(&self) -> Option<Arc<capture::DenyList>> {
        lock(&self.deny_editor).as_ref().map(DenyEditor::list)
    }

    /// `get_deny_list` command.
    pub fn get_deny_list(&self) -> Result<DenyListDto, DenyEditError> {
        lock(&self.deny_editor)
            .as_ref()
            .map(|e| e.view(BANKING_NOTICE))
            .ok_or(DenyEditError::Unavailable)
    }

    /// Run one edit. A deny-list change is a policy change: it takes effect for the NEXT turn
    /// (a running turn keeps the list it started with) and cancels every pending consent.
    fn edit_deny(
        &self,
        f: impl FnOnce(&mut DenyEditor) -> Result<(), DenyEditError>,
    ) -> Result<DenyListDto, DenyEditError> {
        let dto = {
            let mut g = lock(&self.deny_editor);
            let ed = g.as_mut().ok_or(DenyEditError::Unavailable)?;
            f(ed)?;
            ed.view(BANKING_NOTICE)
        };
        self.cancel_pending_consents();
        Ok(dto)
    }

    /// `add_deny_entry` command.
    pub fn add_deny_entry(&self, id: &str) -> Result<DenyListDto, DenyEditError> {
        self.edit_deny(|e| e.add(id))
    }

    /// `remove_deny_entry` command. A built-in entry needs `confirm_remove_default`.
    pub fn remove_deny_entry(
        &self,
        id: &str,
        confirm_remove_default: bool,
    ) -> Result<DenyListDto, DenyEditError> {
        self.edit_deny(|e| e.remove(id, confirm_remove_default))
    }

    /// `reset_deny_list_to_defaults` command: restores removed built-in entries (`confirm`
    /// required); the user's own additions stay.
    pub fn reset_deny_list_to_defaults(&self, confirm: bool) -> Result<DenyListDto, DenyEditError> {
        self.edit_deny(|e| e.reset_to_defaults(confirm))
    }

    // ------------------------------------------------------------ agent activity log (S2-12)

    /// Tell the core the app data directory; the log lives in `<dir>/audit`. If it cannot be
    /// created or written (or is a symlink) the log stays unavailable: `Status.audit_ready` is
    /// `false` and the sink fails every `intent`, so the agent cannot act (fail closed).
    pub fn set_data_dir(&self, dir: PathBuf) {
        self.audit_attempted.store(true, Ordering::SeqCst);
        let _ = self.audit.attach(&dir);
    }

    /// The sink the policy gate must be built with (the same file for the whole run).
    pub fn audit_sink(&self) -> Arc<audit::FileAuditSink> {
        self.audit.clone()
    }

    /// `get_audit_entries` command: newest first, `limit` defaults to 50 (1..=500).
    pub fn get_audit_entries(
        &self,
        cursor: Option<u32>,
        limit: Option<u32>,
    ) -> Result<AuditPageDto, AuditCmdError> {
        let cursor = cursor.unwrap_or(0);
        let page = self
            .audit
            .read_page(u64::from(cursor), limit.unwrap_or(DEFAULT_PAGE) as usize)?;
        let st = self.audit.stats();
        Ok(page_dto(page, st.over_limit, st.ready))
    }

    /// `export_audit_log` command (explicit user action): copy the log into a NEW file at
    /// `dest_path`. Never overwrites; nothing leaves the device.
    pub fn export_audit_log(&self, dest_path: &str) -> Result<AuditExportDto, AuditCmdError> {
        let dest = validate_export_path(dest_path)?;
        let segments = self.audit.export_to(&dest)?;
        Ok(AuditExportDto { segments })
    }

    /// `clear_audit_log` command (explicit user action, `confirm` must be `true`): starts a new
    /// log file whose first line records the clear, then removes the old files. Cancels nothing.
    pub fn clear_audit_log(&self, confirm: bool) -> Result<AuditClearDto, AuditCmdError> {
        let removed = self.audit.clear(confirm)?;
        Ok(AuditClearDto { removed })
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
        let (settings_error, settings_issues) = {
            let st = lock(&self.settings);
            (st.load_error().map(String::from), st.load_issues().to_vec())
        };
        let deny_notices = lock(&self.deny_editor)
            .as_ref()
            .map(DenyEditor::notices)
            .unwrap_or_default();
        let i = lock(&self.inner);
        Status {
            phase: i.phase,
            firewall: i.firewall.clone(),
            hotkey_registered: i.hotkey_registered,
            settings_error,
            settings_issues,
            audit_ready: self.audit.ready(),
            notices: {
                let mut n = lock(&self.notices).clone();
                if self.audit_attempted.load(Ordering::SeqCst) && !self.audit.ready() {
                    n.push(AUDIT_NOTICE.to_string());
                }
                n.extend(deny_notices);
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
        // The edited deny list (if the shell set a config dir) replaces the engine's own for
        // this turn: a change applies to the NEXT turn, never to one in progress.
        let deps = match self.current_deny() {
            Some(deny) => Arc::new(EngineDeps {
                deny,
                ..(*deps).clone()
            }),
            None => deps,
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

    // ------------------------------------------------------------- S2-7

    #[test]
    fn invalid_values_fall_back_report_issues_and_flag_the_hotkey_change() {
        use crate::api::{IssueCode, SettingsField, SettingsIssue};
        let (_d, b) = buddy();
        b.update_settings(&SettingsPatch {
            hotkey: Some("alt+k".into()),
            ..Default::default()
        })
        .unwrap();
        let u = b
            .update_settings(&SettingsPatch {
                hotkey: Some("k".into()),
                anthropic_model: Some("bad model".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(u.settings.hotkey, "Ctrl+Alt+Space");
        assert_eq!(
            u.hotkey_changed,
            Some(("Alt+K".into(), "Ctrl+Alt+Space".into()))
        );
        assert_eq!(
            u.issues,
            vec![
                SettingsIssue {
                    field: SettingsField::Hotkey,
                    code: IssueCode::NeedsModifier
                },
                SettingsIssue {
                    field: SettingsField::AnthropicModel,
                    code: IssueCode::InvalidFormat
                }
            ]
        );
        assert_eq!(u.result().issues, u.issues);
        // The rejected text is nowhere in what the UI gets back.
        assert!(!format!("{:?}", u.result()).contains("bad model"));
    }

    #[test]
    fn load_issues_from_the_file_show_up_in_the_status() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        std::fs::write(&p, br#"{"models_dir":"/nonexistent-root-xyz/a/../b"}"#).unwrap();
        let b = Buddy::new(&p, Box::new(capture::UnsupportedCapture));
        let st = b.get_status();
        assert!(st.settings_error.is_none());
        assert_eq!(st.settings_issues.len(), 1);
        assert_eq!(b.get_settings().models_dir, "");
        // Saving anything clears the visible issue.
        b.update_settings(&SettingsPatch::default()).unwrap();
        assert!(b.get_status().settings_issues.is_empty());
    }

    #[test]
    fn changing_the_models_dir_repoints_the_table_and_the_engine_cell() {
        let (d, b) = buddy();
        let default = d.path().join("default-models");
        b.set_models_dir(default.clone());
        assert_eq!(
            b.get_settings().default_models_dir,
            default.to_string_lossy()
        );
        assert_eq!(b.models_dir(), Some(default.clone()));
        assert_eq!(*lock(&b.models_dir_cell()), default);
        assert_eq!(b.model_manager().unwrap().dir(), default);
        // Nothing is created by choosing a folder.
        assert!(!default.exists());

        let custom = d.path().join("custom");
        let u = b
            .update_settings(&SettingsPatch {
                models_dir: Some(custom.to_str().unwrap().into()),
                ..Default::default()
            })
            .unwrap();
        assert!(u.models_dir_changed && u.issues.is_empty());
        assert_eq!(b.model_manager().unwrap().dir(), custom);
        assert_eq!(*lock(&b.models_dir_cell()), custom);
        assert!(!custom.exists());
        // An unrelated change is not a models change.
        let u = b
            .update_settings(&SettingsPatch {
                local_only: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert!(!u.models_dir_changed);
        // An invalid folder falls back to the default and that IS a change (back to default).
        let u = b
            .update_settings(&SettingsPatch {
                models_dir: Some("../../etc".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(u.models_dir_changed);
        assert_eq!(u.issues.len(), 1);
        assert_eq!(b.model_manager().unwrap().dir(), default);
        assert_eq!(b.get_settings().models_dir, "");
        // Empty = default again, no change.
        let u = b
            .update_settings(&SettingsPatch {
                models_dir: Some(String::new()),
                ..Default::default()
            })
            .unwrap();
        assert!(!u.models_dir_changed);
    }

    #[test]
    fn deny_commands_need_a_config_dir_and_report_in_the_status() {
        let (d, b) = buddy();
        assert_eq!(b.get_deny_list().unwrap_err(), DenyEditError::Unavailable);
        assert_eq!(
            b.add_deny_entry("x.exe").unwrap_err(),
            DenyEditError::Unavailable
        );
        assert!(b.current_deny().is_none());
        b.set_config_dir(d.path().join("cfg"));
        let v = b.get_deny_list().unwrap();
        assert!(v.entries.iter().all(|e| e.builtin));
        assert_eq!(v.banking_notice, BANKING_NOTICE);
        assert!(v.removed_defaults.is_empty() && v.load_issue.is_none());
        let v = b.add_deny_entry("Mine.EXE").unwrap();
        assert!(v.entries.iter().any(|e| e.id == "mine.exe" && !e.builtin));
        assert!(b.current_deny().unwrap().matches("mine.exe"));
        b.remove_deny_entry("cmd.exe", true).unwrap();
        let notices = b.get_status().notices;
        assert!(notices.iter().any(|n| n.contains("cmd.exe")), "{notices:?}");
        assert!(notices.iter().any(|n| n == BANKING_NOTICE));
        b.reset_deny_list_to_defaults(true).unwrap();
        assert!(!b.get_status().notices.iter().any(|n| n.contains("cmd.exe")));
    }

    #[test]
    fn audit_log_is_not_ready_until_the_data_dir_works_and_commands_are_content_free() {
        use policy::AuditSink;
        let (d, b) = buddy();
        assert!(!b.get_status().audit_ready);
        assert!(!b.get_status().notices.iter().any(|n| n == AUDIT_NOTICE));
        assert_eq!(
            b.get_audit_entries(None, None).unwrap_err(),
            AuditCmdError::Unavailable
        );
        assert_eq!(
            b.clear_audit_log(true).unwrap_err(),
            AuditCmdError::Unavailable
        );
        assert!(b.audit_sink().intent(&audit_rec("intent")).is_err());

        // A data dir below a regular file: not ready, the user is told, still fail closed.
        let f = d.path().join("file");
        std::fs::write(&f, b"x").unwrap();
        b.set_data_dir(f.join("data"));
        assert!(!b.get_status().audit_ready);
        assert!(b.get_status().notices.iter().any(|n| n == AUDIT_NOTICE));
        assert!(b.audit_sink().intent(&audit_rec("intent")).is_err());

        b.set_data_dir(d.path().join("data"));
        assert!(b.get_status().audit_ready);
        assert!(!b.get_status().notices.iter().any(|n| n == AUDIT_NOTICE));
        b.audit_sink().intent(&audit_rec("intent")).unwrap();
        b.audit_sink().record(&audit_rec("result"));
        let page = b.get_audit_entries(None, Some(1)).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].event, "result");
        assert_eq!(page.next_cursor, Some(1));
        assert!(page.ready && !page.over_limit && page.corrupt_lines == 0);
        let older = b.get_audit_entries(page.next_cursor, Some(1)).unwrap();
        assert_eq!(older.entries[0].event, "intent");
        assert_eq!(older.next_cursor, None);

        // Export: validated path, new file only.
        let dest = d.path().join("out.jsonl");
        let dest_s = dest.to_string_lossy().into_owned();
        assert_eq!(b.export_audit_log(&dest_s).unwrap().segments, 1);
        assert_eq!(std::fs::read_to_string(&dest).unwrap().lines().count(), 2);
        assert_eq!(
            b.export_audit_log(&dest_s).unwrap_err(),
            AuditCmdError::Exists
        );
        assert_eq!(
            b.export_audit_log("../x.jsonl").unwrap_err(),
            AuditCmdError::InvalidPath
        );

        // Clear needs the explicit confirm.
        assert_eq!(
            b.clear_audit_log(false).unwrap_err(),
            AuditCmdError::NeedsConfirm
        );
        assert_eq!(b.get_audit_entries(None, None).unwrap().entries.len(), 2);
        assert_eq!(b.clear_audit_log(true).unwrap().removed, 1);
        let after = b.get_audit_entries(None, None).unwrap();
        assert_eq!(after.entries.len(), 1);
        assert_eq!(after.entries[0].event, "cleared");
        assert_eq!(after.entries[0].segments_removed, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_audit_dir_keeps_the_log_off() {
        let (d, b) = buddy();
        let real = d.path().join("elsewhere");
        std::fs::create_dir(&real).unwrap();
        let data = d.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::os::unix::fs::symlink(&real, data.join("audit")).unwrap();
        b.set_data_dir(data);
        assert!(!b.get_status().audit_ready);
        assert!(std::fs::read_dir(&real).unwrap().next().is_none());
    }

    fn audit_rec(event: &'static str) -> policy::AuditRecord {
        policy::AuditRecord {
            ts_ms: 1,
            task: Some(1),
            seq: 1,
            event,
            plan_digest8: None,
            action_digest8: None,
            step: Some(0),
            op: Some("click"),
            target_role: None,
            target_label: Some("Save".into()),
            app_id: None,
            tier: Some("reversible"),
            decision: None,
            result_code: None,
            text_len: None,
        }
    }
}
