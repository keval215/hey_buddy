//! Tauri commands: thin wrappers over `Buddy`. Errors are fixed tokens, never content.

use std::sync::Arc;

use buddy_core::api::{Decision, DisplayDto, LastRequestDto, Settings, SettingsPatch, Status};
use buddy_core::Buddy;
use tauri::{AppHandle, State};

use crate::hotkey;

type B<'a> = State<'a, Arc<Buddy>>;

#[tauri::command]
pub fn get_status(b: B) -> Status {
    b.get_status()
}

#[tauri::command]
pub fn get_settings(b: B) -> Settings {
    b.get_settings()
}

#[tauri::command]
pub fn update_settings(app: AppHandle, b: B, patch: SettingsPatch) -> Result<Settings, String> {
    let update = b.update_settings(&patch).map_err(|e| e.to_string())?;
    if let Some((old, new)) = update.hotkey_changed {
        if hotkey::rebind(&app, &old, &new).is_err() {
            // New shortcut unavailable: put the old one back in the settings file too.
            let _ = b.update_settings(&SettingsPatch {
                hotkey: Some(old),
                ..Default::default()
            });
            return Err("hotkey unavailable".into());
        }
        // The new shortcut is live (also heals a failed registration at startup).
        b.set_hotkey_registered(true);
    }
    Ok(update.settings)
}

/// Preview window only (capability `preview`): Send or Cancel for a pending cloud request.
#[tauri::command]
pub fn preview_decision(b: B, request_id: u64, decision: Decision) -> Result<(), String> {
    b.preview_decision(request_id, decision)
        .map_err(|e| e.to_string())
}

/// Settings window only: the most recent turn's request (local or cloud).
#[tauri::command]
pub fn get_last_request(b: B) -> Option<LastRequestDto> {
    b.get_last_request()
}

/// Store the API key in the OS keychain. The key is never logged, stored in settings or
/// returned; errors are fixed tokens (`unsupported_provider`, `invalid_key`,
/// `credential_store_unsupported`, `keychain_error`).
#[tauri::command]
pub fn set_api_key(b: B, provider: String, key: String) -> Result<Settings, String> {
    b.set_api_key(&provider, key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_api_key(b: B, provider: String) -> Result<Settings, String> {
    b.clear_api_key(&provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_displays(b: B) -> Vec<DisplayDto> {
    b.list_displays()
}

#[tauri::command]
pub fn cancel_turn(b: B) {
    b.cancel_turn();
}

/// Dev builds only (not registered in release): emit a `turn:point` for manual overlay tests.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn debug_point(app: AppHandle, b: B, x: i32, y: i32, label: String) -> Result<(), String> {
    use tauri::Emitter;
    let p = b
        .debug_point(x, y, &label)
        .ok_or_else(|| "no display contains that point".to_string())?;
    app.emit(buddy_core::api::EVT_TURN_POINT, p)
        .map_err(|_| "emit failed".to_string())
}
