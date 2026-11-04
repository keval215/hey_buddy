//! hey_buddy desktop shell (T10): tray, settings window, push-to-talk hotkey, per-display
//! click-through overlay. Thin glue: all logic lives in `buddy-core` (ADR 0001).
//!
//! Privacy: overlays, the settings window, the preview panel and the approval window are excluded from screen capture (Windows:
//! `exclude_window_from_capture`; macOS: the capture crate excludes this process by pid).
//! Nothing here logs labels or any model/user content.

// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod approval;
mod commands;
mod engine;
mod hotkey;
mod kill;
mod overlay;
mod preview;
mod shell;
mod updates;

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
            // The kill switch exists before anything else can start (S2-13).
            app.manage(kill::KillHotkey::new());
            let ctl = buddy_core::kill::KillController::for_buddy(&buddy);
            // The approval broker is stopped by the same switch (S2-16). The supervisor (S2-23)
            // asks it; the approval window can only answer.
            app.manage(buddy_core::approval::ApprovalBroker::with_system_clock(
                ctl.gate().kill_switch(),
            ));
            app.manage(ctl);
            // Enables the models screen. Creates nothing and downloads nothing by itself.
            if let Ok(data) = app.path().app_data_dir() {
                buddy.set_models_dir(buddy_core::download::models_dir(&data));
                // The activity log (<data>/audit). If it cannot be written the agent stays off.
                buddy.set_data_dir(data);
            }
            shell::setup(app.handle(), &buddy)?;
            engine::start(app.handle(), &buddy, &dir);
            agent::start(app.handle(), &buddy);
            updates::setup(app.handle(), &buddy);
            Ok(())
        });

    #[cfg(debug_assertions)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::get_status,
        commands::get_settings,
        commands::update_settings,
        commands::request_engine_swap,
        commands::list_displays,
        commands::cancel_turn,
        commands::debug_point,
        commands::debug_fake_task,
        commands::preview_decision,
        commands::get_last_request,
        commands::get_pending_preview,
        commands::set_api_key,
        commands::clear_api_key,
        commands::list_models,
        commands::start_download,
        commands::cancel_download,
        commands::get_deny_list,
        commands::add_deny_entry,
        commands::remove_deny_entry,
        commands::reset_deny_list_to_defaults,
        commands::get_audit_entries,
        commands::export_audit_log,
        commands::clear_audit_log,
        commands::kill_agent,
        commands::approval_ready,
        commands::approval_displayed,
        commands::approval_respond,
        commands::agent_start_task,
        commands::agent_status,
        commands::update_status,
        commands::update_check,
        commands::update_download,
        commands::update_cancel,
        commands::update_install,
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::get_status,
        commands::get_settings,
        commands::update_settings,
        commands::request_engine_swap,
        commands::list_displays,
        commands::cancel_turn,
        commands::preview_decision,
        commands::get_last_request,
        commands::get_pending_preview,
        commands::set_api_key,
        commands::clear_api_key,
        commands::list_models,
        commands::start_download,
        commands::cancel_download,
        commands::get_deny_list,
        commands::add_deny_entry,
        commands::remove_deny_entry,
        commands::reset_deny_list_to_defaults,
        commands::get_audit_entries,
        commands::export_audit_log,
        commands::clear_audit_log,
        commands::kill_agent,
        commands::approval_ready,
        commands::approval_displayed,
        commands::approval_respond,
        commands::agent_start_task,
        commands::agent_status,
        commands::update_status,
        commands::update_check,
        commands::update_download,
        commands::update_cancel,
        commands::update_install,
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
