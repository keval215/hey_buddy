//! Tauri commands: thin wrappers over `Buddy`. Errors are fixed tokens, never content.

use std::sync::Arc;

use buddy_core::api::{DisplayDto, Settings, SettingsPatch, Status};
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
    }
    Ok(update.settings)
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
