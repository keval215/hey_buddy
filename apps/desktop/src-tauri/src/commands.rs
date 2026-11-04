//! Tauri commands: thin wrappers over `Buddy`. Errors are fixed tokens, never content.

use std::sync::Arc;

use buddy_core::api::{
    Decision, DenyListDto, DisplayDto, IssueCode, LastRequestDto, ModelRow, PreviewDto, Settings,
    SettingsField, SettingsIssue, SettingsPatch, SettingsResult, Status, SwapTarget,
};
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

/// Settings window only. Invalid values never fail the call: they fall back to the safest
/// value and come back as typed issues. An OS refusal of the new shortcut puts the old one
/// back and adds a `hotkey` / `unavailable` issue. Only a failed write is an error.
#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    b: B,
    patch: SettingsPatch,
) -> Result<SettingsResult, String> {
    let update = b.update_settings(&patch).map_err(|e| e.to_string())?;
    let mut result = update.result();
    if let Some((old, new)) = update.hotkey_changed {
        if hotkey::rebind(&app, &old, &new).is_err() {
            // New shortcut unavailable: put the old one back in the settings file too.
            if let Ok(back) = b.update_settings(&SettingsPatch {
                hotkey: Some(old),
                ..Default::default()
            }) {
                result.settings = back.settings;
            }
            result.issues.push(SettingsIssue {
                field: SettingsField::Hotkey,
                code: IssueCode::Unavailable,
            });
            return Ok(result);
        }
        // The new shortcut is live (also heals a failed registration at startup).
        b.set_hotkey_registered(true);
    }
    Ok(result)
}

/// Settings window only: (re)load the STT/TTS engines of the current profile in the
/// background. Returns at once; progress arrives as `engine_swap_requested`,
/// `engine_swap_done` and `engine_swap_failed` events. A running turn is never disturbed: the
/// new engine is applied after it ends. Errors are fixed tokens (`invalid_model`,
/// `loader_unavailable`); `slot` is `stt`, `tts` or `all` (default).
#[tauri::command]
pub fn request_engine_swap(b: B, slot: Option<SwapTarget>) -> Result<(), String> {
    b.request_engine_swap(slot.unwrap_or_default())
        .map_err(|e| e.to_string())
}

/// Preview window only (capability `preview`): Send or Cancel for a pending cloud request.
#[tauri::command]
pub fn preview_decision(b: B, request_id: u64, decision: Decision) -> Result<(), String> {
    b.preview_decision(request_id, decision)
        .map_err(|e| e.to_string())
}

/// Preview window only (capability `preview`): the preview currently waiting for a decision,
/// so a panel that mounts after `turn:preview` was emitted still shows it.
#[tauri::command]
pub fn get_pending_preview(b: B) -> Option<PreviewDto> {
    b.pending_preview()
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

/// Settings window only: the models table (status, size, licence, disk needed / free). Reads
/// the local filesystem only; never touches the network.
#[tauri::command]
pub fn list_models(b: B) -> Vec<ModelRow> {
    b.list_models()
}

/// Settings window only, explicit click: download one registry model in the background.
/// `accept_unknown_checksum` is the second confirm for entries whose checksum is unknown.
/// Returns at once; progress arrives as `download_progress`, then `download_done` or
/// `download_failed`. Errors are fixed tokens (`busy`, `no_source`, `checksum_unknown`,
/// `disk_space`, ...).
#[tauri::command]
pub fn start_download(
    b: B,
    id: String,
    accept_unknown_checksum: Option<bool>,
) -> Result<(), String> {
    b.start_download(&id, accept_unknown_checksum.unwrap_or(false))
        .map_err(|e| e.to_string())
}

/// Settings window only: stop the running download of `id`; its partial file is deleted.
#[tauri::command]
pub fn cancel_download(b: B, id: String) {
    b.cancel_download(&id);
}

/// Settings window only: the deny-list editor's view (built-in and user entries, removed
/// built-ins, the banking notice, a load problem if any).
#[tauri::command]
pub fn get_deny_list(b: B) -> Result<DenyListDto, String> {
    b.get_deny_list().map_err(|e| e.to_string())
}

/// Settings window only: add an app id (`[A-Za-z0-9._-]{1,128}`, stored lowercase). Takes
/// effect on the next turn and cancels any pending preview. Errors are fixed tokens
/// (`invalid_id`, `already_listed`, `io_error`, ...).
#[tauri::command]
pub fn add_deny_entry(b: B, id: String) -> Result<DenyListDto, String> {
    b.add_deny_entry(&id).map_err(|e| e.to_string())
}

/// Settings window only: remove an app id. A built-in entry needs
/// `confirm_remove_default: true` (`needs_confirm` otherwise); the list can never become
/// empty (`would_be_empty`).
#[tauri::command]
pub fn remove_deny_entry(
    b: B,
    id: String,
    confirm_remove_default: Option<bool>,
) -> Result<DenyListDto, String> {
    b.remove_deny_entry(&id, confirm_remove_default.unwrap_or(false))
        .map_err(|e| e.to_string())
}

/// Settings window only: bring back every removed built-in entry (needs `confirm: true`);
/// the user's own additions stay.
#[tauri::command]
pub fn reset_deny_list_to_defaults(b: B, confirm: Option<bool>) -> Result<DenyListDto, String> {
    b.reset_deny_list_to_defaults(confirm.unwrap_or(false))
        .map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    const NEW_COMMANDS: &[&str] = &[
        "get_deny_list",
        "add_deny_entry",
        "remove_deny_entry",
        "reset_deny_list_to_defaults",
    ];

    /// The quoted strings of the `"permissions": [...]` array (capability files hold no
    /// escapes there; keeps this test free of a JSON dependency).
    fn permissions(json: &str) -> BTreeSet<String> {
        let start = json.find("\"permissions\"").expect("permissions key");
        let open = start + json[start..].find('[').expect("array start");
        let close = open + json[open..].find(']').expect("array end");
        json[open + 1..close]
            .split('"')
            .enumerate()
            .filter(|(i, _)| i % 2 == 1)
            .map(|(_, s)| s.to_string())
            .collect()
    }

    fn allow(cmd: &str) -> String {
        format!("allow-{}", cmd.replace('_', "-"))
    }

    #[test]
    fn deny_list_commands_are_only_in_the_settings_window_capability() {
        let default = permissions(include_str!("../capabilities/default.json"));
        let overlay = permissions(include_str!("../capabilities/overlay.json"));
        let preview = permissions(include_str!("../capabilities/preview.json"));
        for cmd in NEW_COMMANDS {
            let p = allow(cmd);
            assert!(default.contains(&p), "default.json must allow {p}");
            assert!(!overlay.contains(&p), "overlay.json must not allow {p}");
            assert!(!preview.contains(&p), "preview.json must not allow {p}");
        }
        // The overlay can call nothing; the preview panel only its two commands.
        assert!(overlay.iter().all(|p| p.starts_with("core:event:")));
        let preview_cmds: BTreeSet<_> =
            preview.iter().filter(|p| p.starts_with("allow-")).collect();
        assert_eq!(
            preview_cmds,
            ["allow-preview-decision", "allow-get-pending-preview"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
                .iter()
                .collect()
        );
    }

    #[test]
    fn every_settings_command_is_declared_to_the_build_and_the_handler() {
        let build = include_str!("../build.rs");
        let main = include_str!("main.rs");
        for cmd in NEW_COMMANDS {
            assert!(
                build.contains(&format!("\"{cmd}\"")),
                "build.rs lacks {cmd}"
            );
            assert_eq!(
                main.matches(&format!("commands::{cmd},")).count(),
                2,
                "main.rs must register {cmd} in both handler lists"
            );
        }
        // Everything declared in build.rs is granted to the settings window except the
        // two preview-window commands, which the preview capability has instead.
        let default = permissions(include_str!("../capabilities/default.json"));
        let preview = permissions(include_str!("../capabilities/preview.json"));
        for cmd in ["preview_decision", "get_pending_preview"] {
            assert!(preview.contains(&allow(cmd)));
            assert!(!default.contains(&allow(cmd)), "{cmd} is preview-only");
        }
    }
}
