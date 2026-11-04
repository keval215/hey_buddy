//! Tray icon, settings window, display watcher, and event emission.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buddy_core::api::{
    DownloadDone, DownloadFailed, DownloadProgress, EngineSwapEvent, FirewallStatus, Phase,
    PointEvent, ReplyDelta, TranscriptEvent, TurnError, TurnState, EVT_DOWNLOAD_DONE,
    EVT_DOWNLOAD_FAILED, EVT_DOWNLOAD_PROGRESS, EVT_ENGINE_SWAP_DONE, EVT_ENGINE_SWAP_FAILED,
    EVT_ENGINE_SWAP_REQUESTED, EVT_FIREWALL_STATUS, EVT_TURN_ERROR, EVT_TURN_POINT,
    EVT_TURN_REPLY_DELTA, EVT_TURN_STATE, EVT_TURN_TRANSCRIPT,
};
use buddy_core::{Buddy, EventSink};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::{hotkey, overlay, preview};

const MAIN: &str = "main";
const DISPLAY_POLL: Duration = Duration::from_secs(3);

/// The disabled status line in the tray menu.
struct StatusItem(MenuItem<Wry>);
static GENERATION: AtomicU32 = AtomicU32::new(0);

fn phase_text(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "Status: idle",
        Phase::Listening => "Status: listening",
        Phase::Transcribing => "Status: transcribing",
        Phase::Thinking => "Status: thinking",
        Phase::Speaking => "Status: speaking",
    }
}

/// Turns core events into Tauri events (ADR 0001). Called from any thread.
///
/// Privacy: `turn:transcript` and `turn:reply_delta` carry what the user said and what the
/// buddy answered, so they go to the settings window only (never the overlays or the preview
/// panel). Everything else is fixed tokens, points and states.
pub struct TauriEvents(pub AppHandle);

impl EventSink for TauriEvents {
    fn turn_state(&self, ev: TurnState) {
        // Queue the tray update instead of waiting for the main thread: the caller may hold
        // locks the main thread is waiting for (hotkey handler).
        let app = self.0.clone();
        let phase = ev.phase;
        let _ = self.0.run_on_main_thread(move || {
            if let Some(item) = app.try_state::<StatusItem>() {
                let _ = item.0.set_text(phase_text(phase));
            }
        });
        let _ = self.0.emit(EVT_TURN_STATE, ev);
    }
    fn transcript(&self, ev: TranscriptEvent) {
        let _ = self.0.emit_to(MAIN, EVT_TURN_TRANSCRIPT, ev);
    }
    fn reply_delta(&self, ev: ReplyDelta) {
        let _ = self.0.emit_to(MAIN, EVT_TURN_REPLY_DELTA, ev);
    }
    fn point(&self, ev: PointEvent) {
        let _ = self.0.emit(EVT_TURN_POINT, ev);
    }
    fn firewall_status(&self, st: FirewallStatus) {
        let _ = self.0.emit(EVT_FIREWALL_STATUS, st);
    }
    fn error(&self, e: TurnError) {
        let _ = self.0.emit(EVT_TURN_ERROR, e);
    }
    // Engine swaps: model id, fixed error token and the expected model path only. Settings
    // window only (the overlays and the preview panel have no use for them).
    fn engine_swap_requested(&self, ev: EngineSwapEvent) {
        let _ = self.0.emit_to(MAIN, EVT_ENGINE_SWAP_REQUESTED, ev);
    }
    fn engine_swap_done(&self, ev: EngineSwapEvent) {
        let _ = self.0.emit_to(MAIN, EVT_ENGINE_SWAP_DONE, ev);
    }
    fn engine_swap_failed(&self, ev: EngineSwapEvent) {
        let _ = self.0.emit_to(MAIN, EVT_ENGINE_SWAP_FAILED, ev);
    }
    // Model downloads: id, byte counts and fixed error tokens only; settings window only.
    fn download_progress(&self, ev: DownloadProgress) {
        let _ = self.0.emit_to(MAIN, EVT_DOWNLOAD_PROGRESS, ev);
    }
    fn download_done(&self, ev: DownloadDone) {
        let _ = self.0.emit_to(MAIN, EVT_DOWNLOAD_DONE, ev);
    }
    fn download_failed(&self, ev: DownloadFailed) {
        let _ = self.0.emit_to(MAIN, EVT_DOWNLOAD_FAILED, ev);
    }
}

pub fn setup(app: &AppHandle, buddy: &Arc<Buddy>) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    // Settings window: hidden at start, hidden (not destroyed) on close, never captured.
    if let Some(w) = app.get_webview_window(MAIN) {
        if !overlay::protect(&w) {
            let _ = w.destroy();
        } else {
            let w2 = w.clone();
            w.on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    let _ = w2.hide();
                }
            });
        }
    }

    let status = MenuItem::with_id(app, "status", phase_text(Phase::Idle), false, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &quit,
        ],
    )?;
    app.manage(StatusItem(status));
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("hey_buddy")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    // Preview panel: the core shows cloud previews through it (no panel = no cloud, fail closed).
    buddy.set_preview_sink(Arc::new(preview::TauriSink(app.clone())));
    preview::create(app, buddy)?;

    hotkey::register_initial(app, buddy);
    start_display_watcher(app, buddy);
    Ok(())
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Build the overlays now and rebuild them whenever the display list changes (polled: there is
/// no portable monitor-change event in tauri).
fn start_display_watcher(app: &AppHandle, buddy: &Arc<Buddy>) {
    let last = Arc::new(Mutex::new(buddy.list_displays()));
    overlay::rebuild(
        app,
        &last.lock().unwrap(),
        GENERATION.fetch_add(1, Ordering::Relaxed),
    );
    let (app, buddy) = (app.clone(), buddy.clone());
    std::thread::spawn(move || loop {
        std::thread::sleep(DISPLAY_POLL);
        let now = buddy.list_displays();
        let mut guard = last.lock().unwrap();
        if *guard == now {
            continue;
        }
        *guard = now.clone();
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            overlay::rebuild(&handle, &now, GENERATION.fetch_add(1, Ordering::Relaxed));
        });
    });
}
