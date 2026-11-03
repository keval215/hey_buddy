//! Tray icon, settings window, display watcher, and event emission.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buddy_core::api::{Phase, TurnState, EVT_TURN_STATE};
use buddy_core::Buddy;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::{hotkey, overlay};

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

/// Emit `turn:state` and refresh the tray status line.
pub fn emit_turn_state(app: &AppHandle, ev: TurnState) {
    if let Some(item) = app.try_state::<StatusItem>() {
        let _ = item.0.set_text(phase_text(ev.phase));
    }
    let _ = app.emit(EVT_TURN_STATE, ev);
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
