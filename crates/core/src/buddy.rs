//! `Buddy`: the app-level state the Tauri glue talks to (ADR 0001: the app depends only on
//! this crate). T10 skeleton: settings, displays, hotkey state. The turn pipeline (capture ->
//! firewall -> STT -> LLM -> TTS) is wired in T12 at the marked hooks.
//!
//! Every method returns the events to emit; the glue does the emitting. No audio, frames or
//! transcripts pass through here in T10. Push-to-talk only: nothing here opens a microphone.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use capture::CaptureSource;
use llm::{
    CloudOptIn, CredentialStore, LlmProvider, LlmRequest, Locality, PreparedRequest,
    RequestPreview, ANTHROPIC_ACCOUNT, ANTHROPIC_PROVIDER_ID,
};
use secrecy::SecretString;

use crate::api::{
    Decision, DisplayDto, FirewallStatus, LastRequestDto, Phase, PointEvent, Settings,
    SettingsPatch, Status, TurnState,
};
use crate::consent::{
    last_request_dto, provider_allowed, ConsentError, ConsentGate, PreviewSink, PreviewStore,
    LAST_KEY,
};
use crate::error::{CoreError, KeyError};
use crate::point::sanitize_label;
use crate::settings::{SettingsError, SettingsStore};

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

/// Longest accepted API key (bytes).
const MAX_KEY_LEN: usize = 512;

pub struct Buddy {
    settings: Mutex<SettingsStore>,
    inner: Mutex<Inner>,
    capture: Box<dyn CaptureSource>,
    store: Arc<PreviewStore>,
    pub(crate) consent: Arc<ConsentGate>,
    credentials: Arc<dyn CredentialStore>,
    /// Cached "a key is stored" (best effort); refreshed after set/clear.
    has_key: Mutex<Option<bool>>,
    /// Most recent turn's request (in memory only, replaced each turn).
    last: Mutex<Option<LastRequestDto>>,
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
        Self {
            settings: Mutex::new(settings),
            inner: Mutex::new(Inner {
                turn_id: 0,
                phase: Phase::Idle,
                firewall: FirewallStatus::default(),
                hotkey_registered: false,
            }),
            capture,
            store,
            consent,
            credentials,
            has_key: Mutex::new(None),
            last: Mutex::new(None),
        }
    }

    fn has_api_key(&self) -> bool {
        let mut c = lock(&self.has_key);
        *c.get_or_insert_with(|| matches!(self.credentials.get(ANTHROPIC_ACCOUNT), Ok(Some(_))))
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

    /// Push-to-talk pressed. Returns the `turn:state` to emit, or `None` when a turn is already
    /// running (key auto-repeat, or pressed during a turn).
    ///
    /// T12 HOOK: start the turn here (preflight, route, capture at press time, start the mic).
    pub fn on_hotkey_pressed(&self) -> Option<TurnState> {
        let ev = {
            let mut i = lock(&self.inner);
            if i.phase != Phase::Idle {
                return None;
            }
            i.turn_id = i.turn_id.wrapping_add(1);
            i.phase = Phase::Listening;
            TurnState {
                turn_id: i.turn_id,
                phase: i.phase,
            }
        };
        // A new turn supersedes any preview still waiting from an earlier one.
        self.consent.cancel_all();
        Some(ev)
    }

    /// Push-to-talk released. T10: straight back to `Idle`.
    ///
    /// T12 HOOK: stop the mic and move to `Transcribing` instead of `Idle`.
    pub fn on_hotkey_released(&self) -> Option<TurnState> {
        let mut i = lock(&self.inner);
        if i.phase != Phase::Listening {
            return None;
        }
        i.phase = Phase::Idle;
        Some(TurnState {
            turn_id: i.turn_id,
            phase: i.phase,
        })
    }

    /// `cancel_turn` command. T12 HOOK: also stop STT/LLM/TTS. Pending previews are cancelled.
    pub fn cancel_turn(&self) {
        self.consent.cancel_all();
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
    fn hotkey_press_release_cycle() {
        let (_d, b) = buddy();
        assert_eq!(b.get_status().phase, Phase::Idle);
        let p = b.on_hotkey_pressed().unwrap();
        assert_eq!((p.turn_id, p.phase), (1, Phase::Listening));
        assert_eq!(b.get_status().phase, Phase::Listening);
        // Auto-repeat press is ignored.
        assert!(b.on_hotkey_pressed().is_none());
        let r = b.on_hotkey_released().unwrap();
        assert_eq!((r.turn_id, r.phase), (1, Phase::Idle));
        // Release without press is ignored.
        assert!(b.on_hotkey_released().is_none());
        assert_eq!(b.on_hotkey_pressed().unwrap().turn_id, 2);
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
