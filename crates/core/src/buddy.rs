//! `Buddy`: the app-level state the Tauri glue talks to (ADR 0001: the app depends only on
//! this crate). T10 skeleton: settings, displays, hotkey state. The turn pipeline (capture ->
//! firewall -> STT -> LLM -> TTS) is wired in T12 at the marked hooks.
//!
//! Every method returns the events to emit; the glue does the emitting. No audio, frames or
//! transcripts pass through here in T10. Push-to-talk only: nothing here opens a microphone.

use std::path::PathBuf;
use std::sync::Mutex;

use capture::CaptureSource;

use crate::api::{
    DisplayDto, FirewallStatus, Phase, PointEvent, Settings, SettingsPatch, Status, TurnState,
};
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

pub struct Buddy {
    settings: Mutex<SettingsStore>,
    inner: Mutex<Inner>,
    capture: Box<dyn CaptureSource>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Buddy {
    pub fn new(settings_path: impl Into<PathBuf>, capture: Box<dyn CaptureSource>) -> Self {
        Self {
            settings: Mutex::new(SettingsStore::load(settings_path)),
            inner: Mutex::new(Inner {
                turn_id: 0,
                phase: Phase::Idle,
                firewall: FirewallStatus::default(),
                hotkey_registered: false,
            }),
            capture,
        }
    }

    pub fn get_settings(&self) -> Settings {
        lock(&self.settings).settings()
    }

    pub fn update_settings(&self, patch: &SettingsPatch) -> Result<SettingsUpdate, SettingsError> {
        let mut store = lock(&self.settings);
        let old = store.settings().hotkey;
        let settings = store.apply(patch)?;
        let hotkey_changed = (settings.hotkey != old).then(|| (old, settings.hotkey.clone()));
        Ok(SettingsUpdate {
            settings,
            hotkey_changed,
        })
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
        let mut i = lock(&self.inner);
        if i.phase != Phase::Idle {
            return None;
        }
        i.turn_id = i.turn_id.wrapping_add(1);
        i.phase = Phase::Listening;
        Some(TurnState {
            turn_id: i.turn_id,
            phase: i.phase,
        })
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

    /// `cancel_turn` command: no-op stub until T12.
    pub fn cancel_turn(&self) {}

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
