//! hey_buddy desktop shell (T10): tray, settings window, push-to-talk hotkey, per-display
//! click-through overlay. Thin glue: all logic lives in `buddy-core` (ADR 0001).
//!
//! Privacy: overlays, the settings window and the preview panel are excluded from screen capture (Windows:
//! `exclude_window_from_capture`; macOS: the capture crate excludes this process by pid).
//! Nothing here logs labels or any model/user content.

// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod engine;
mod hotkey;
mod overlay;
mod preview;
mod shell;

use std::sync::Arc;

use buddy_core::Buddy;
use tauri::Manager;

fn main() {
    let builder = tauri::Builder::default()
        .plugin(hotkey::plugin())
        .register_uri_scheme_protocol(preview::SCHEME, preview::handle)
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            let buddy = Arc::new(Buddy::new(
                dir.join("settings.json"),
                buddy_core::platform_capture(),
            ));
            buddy.set_event_sink(Arc::new(shell::TauriEvents(app.handle().clone())));
            app.manage(buddy.clone());
            shell::setup(app.handle(), &buddy)?;
            engine::start(app.handle(), &buddy, &dir);
            Ok(())
        });

    #[cfg(debug_assertions)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::get_status,
        commands::get_settings,
        commands::update_settings,
        commands::list_displays,
        commands::cancel_turn,
        commands::debug_point,
        commands::preview_decision,
        commands::get_last_request,
        commands::get_pending_preview,
        commands::set_api_key,
        commands::clear_api_key,
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::get_status,
        commands::get_settings,
        commands::update_settings,
        commands::list_displays,
        commands::cancel_turn,
        commands::preview_decision,
        commands::get_last_request,
        commands::get_pending_preview,
        commands::set_api_key,
        commands::clear_api_key,
    ]);

    builder
        .build(tauri::generate_context!())
        .expect("error while building hey_buddy")
        .run(|app, event| {
            // Quitting cancels any pending cloud preview (nothing is ever sent on quit).
            if let tauri::RunEvent::Exit = event {
                if let Some(b) = app.try_state::<Arc<Buddy>>() {
                    b.cancel_pending_consents();
                }
            }
        });
}
