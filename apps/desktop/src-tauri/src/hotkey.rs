//! Global push-to-talk hotkey (`tauri-plugin-global-shortcut`). Pressed/Released go straight to
//! `Buddy::on_hotkey_pressed/released` (the turn engine).

use std::sync::Arc;

use buddy_core::Buddy;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            let Some(buddy) = app.try_state::<Arc<Buddy>>() else {
                return;
            };
            // The turn engine reports its state through the event sink (shell::TauriEvents).
            match event.state() {
                ShortcutState::Pressed => buddy.on_hotkey_pressed(),
                ShortcutState::Released => buddy.on_hotkey_released(),
            }
        })
        .build()
}

/// Register the saved shortcut at startup; the result shows up in `Status.hotkey_registered`.
pub fn register_initial(app: &AppHandle, buddy: &Buddy) {
    let ok = app
        .global_shortcut()
        .register(buddy.get_settings().hotkey.as_str())
        .is_ok();
    if !ok {
        eprintln!("hey_buddy: could not register the push-to-talk hotkey");
    }
    buddy.set_hotkey_registered(ok);
}

/// Switch to `new`. The old shortcut stays active when `new` cannot be registered.
pub fn rebind(app: &AppHandle, old: &str, new: &str) -> Result<(), ()> {
    let gs = app.global_shortcut();
    gs.register(new).map_err(|_| ())?;
    let _ = gs.unregister(old);
    Ok(())
}
