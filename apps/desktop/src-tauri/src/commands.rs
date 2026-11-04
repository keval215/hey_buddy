//! Tauri commands: thin wrappers over `Buddy`. Errors are fixed tokens, never content.

use std::sync::Arc;

use buddy_core::api::{
    AuditClearDto, AuditExportDto, AuditPageDto, Decision, DenyListDto, DisplayDto, IssueCode,
    LastRequestDto, ModelRow, PreviewDto, Settings, SettingsField, SettingsIssue, SettingsPatch,
    SettingsResult, Status, SwapTarget,
};
use buddy_core::approval::{
    ApprovalBroker, ApprovalDecisionDto, ApprovalRequestDto, APPROVAL_WINDOW,
};
use buddy_core::kill::{KillController, KillReason};
use buddy_core::Buddy;
use tauri::{AppHandle, Manager, State};

use crate::{hotkey, kill};

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
    if let Some((old, new)) = update.kill_hotkey_changed {
        let ctl = app
            .try_state::<Arc<KillController>>()
            .map(|c| Arc::clone(&c));
        let ok = ctl
            .as_ref()
            .is_some_and(|c| kill::rebind(&app, c, Some(&old), &new).is_ok());
        if ok {
            b.set_kill_hotkey_registered(true);
        } else {
            // The old chord stays registered; put it back in the settings file too.
            if let Ok(back) = b.update_settings(&SettingsPatch {
                kill_hotkey: Some(old),
                ..Default::default()
            }) {
                result.settings = back.settings;
            }
            result.issues.push(SettingsIssue {
                field: SettingsField::KillHotkey,
                code: IssueCode::Unavailable,
            });
        }
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

/// Settings window only: a page of the agent activity log, newest first (`cursor` = how many
/// entries to skip, `limit` default 50). Content-free. Errors are fixed tokens (`unavailable`).
#[tauri::command]
pub fn get_audit_entries(
    b: B,
    cursor: Option<u32>,
    limit: Option<u32>,
) -> Result<AuditPageDto, String> {
    b.get_audit_entries(cursor, limit)
        .map_err(|e| e.to_string())
}

/// Settings window only, explicit click: copy the activity log into a NEW file at `dest_path`
/// (absolute, no overwrite). Errors: `invalid_path`, `exists`, `unavailable`, `io_error`.
#[tauri::command]
pub fn export_audit_log(b: B, dest_path: String) -> Result<AuditExportDto, String> {
    b.export_audit_log(&dest_path).map_err(|e| e.to_string())
}

/// Settings window only, explicit click, needs `confirm: true` (`needs_confirm` otherwise):
/// start a new log whose first line records the clear, then remove the old files.
#[tauri::command]
pub fn clear_audit_log(b: B, confirm: Option<bool>) -> Result<AuditClearDto, String> {
    b.clear_audit_log(confirm.unwrap_or(false))
        .map_err(|e| e.to_string())
}

/// ANY app window (settings, overlay, preview, approval): STOP everything. It can only stop
/// things, so it is safe to expose everywhere. Never blocks: the epoch moves at once and the
/// kill worker does the rest. Returns nothing (no state leaks to a compromised page).
#[tauri::command]
pub fn kill_agent(ctl: State<Arc<KillController>>) {
    ctl.trip(KillReason::StopButton);
}

/// Approval window only (capability `approval`; also checked by label here): the approvals that
/// are waiting, oldest first, so a window that mounts after `approval:request` still shows them.
/// The window can only read and answer approvals core created; it cannot create one.
#[tauri::command]
pub fn approval_ready(
    window: tauri::Window,
    broker: State<ApprovalBroker>,
) -> Result<Vec<ApprovalRequestDto>, String> {
    only_approval_window(&window)?;
    Ok(broker.pending_requests())
}

/// Approval window only: this request is on screen now (starts the 500 ms input guard).
#[tauri::command]
pub fn approval_displayed(
    window: tauri::Window,
    broker: State<ApprovalBroker>,
    id: String,
) -> Result<(), String> {
    only_approval_window(&window)?;
    broker.mark_displayed(&id).map_err(|e| e.to_string())
}

/// Approval window only: Approve or Deny a request core created. Refused (a fixed token, the
/// request stays pending) for an unknown id, a wrong digest, before the display acknowledgement
/// or inside the 500 ms guard.
#[tauri::command]
pub fn approval_respond(
    window: tauri::Window,
    broker: State<ApprovalBroker>,
    id: String,
    decision: ApprovalDecisionDto,
    echoed_digest: String,
) -> Result<(), String> {
    only_approval_window(&window)?;
    broker
        .respond(&id, decision, &echoed_digest)
        .map_err(|e| e.to_string())
}

/// Defence in depth behind the per-window capability.
fn only_approval_window(window: &tauri::Window) -> Result<(), String> {
    if window.label() == APPROVAL_WINDOW {
        Ok(())
    } else {
        Err("forbidden".to_string())
    }
}

/// Dev builds only: start a fake long task (waits for the kill switch) for manual tests.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn debug_fake_task(ctl: State<Arc<KillController>>) {
    kill::debug_task::start(&ctl);
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
        "get_audit_entries",
        "export_audit_log",
        "clear_audit_log",
    ];

    /// Commands every window may call (they can only stop things).
    const EVERYWHERE: &[&str] = &["kill_agent"];

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
    fn settings_only_commands_are_only_in_the_settings_window_capability() {
        let default = permissions(include_str!("../capabilities/default.json"));
        let overlay = permissions(include_str!("../capabilities/overlay.json"));
        let preview = permissions(include_str!("../capabilities/preview.json"));
        for cmd in NEW_COMMANDS {
            let p = allow(cmd);
            assert!(default.contains(&p), "default.json must allow {p}");
            assert!(!overlay.contains(&p), "overlay.json must not allow {p}");
            assert!(!preview.contains(&p), "preview.json must not allow {p}");
        }
        // `kill_agent` (stop only) is callable from every window.
        for cmd in EVERYWHERE {
            let p = allow(cmd);
            assert!(default.contains(&p), "default.json must allow {p}");
            assert!(overlay.contains(&p), "overlay.json must allow {p}");
            assert!(preview.contains(&p), "preview.json must allow {p}");
        }
        // The overlay can call nothing but the stop command; the preview panel only its two
        // commands and the stop command.
        assert!(overlay
            .iter()
            .all(|p| p.starts_with("core:event:") || p == "allow-kill-agent"));
        let preview_cmds: BTreeSet<_> = preview
            .iter()
            .filter(|p| p.starts_with("allow-") && *p != "allow-kill-agent")
            .collect();
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
        for cmd in EVERYWHERE {
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

    /// The approval window's commands (S2-16): the window may call these and nothing else.
    const APPROVAL_COMMANDS: &[&str] =
        &["approval_ready", "approval_displayed", "approval_respond"];

    #[test]
    fn approval_capability_grants_exactly_the_allowed_permissions() {
        let approval = permissions(include_str!("../capabilities/approval.json"));
        let expected: BTreeSet<String> = [
            "core:event:allow-listen",
            "core:event:allow-unlisten",
            "allow-approval-ready",
            "allow-approval-displayed",
            "allow-approval-respond",
            "allow-kill-agent",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(approval, expected);
        // Only the approval window, nothing else (no wildcard).
        let json = include_str!("../capabilities/approval.json");
        assert!(json.contains("\"windows\": [\"approval\"]"));
        for forbidden in [
            "fs:",
            "shell:",
            "http:",
            "dialog:",
            "opener:",
            "process:",
            "core:window",
            "core:webview",
        ] {
            assert!(
                !json.contains(forbidden),
                "approval.json must not grant {forbidden}"
            );
        }
    }

    #[test]
    fn approval_commands_are_only_in_the_approval_capability() {
        let default = permissions(include_str!("../capabilities/default.json"));
        let overlay = permissions(include_str!("../capabilities/overlay.json"));
        let preview = permissions(include_str!("../capabilities/preview.json"));
        let approval = permissions(include_str!("../capabilities/approval.json"));
        let build = include_str!("../build.rs");
        let main = include_str!("main.rs");
        for cmd in APPROVAL_COMMANDS {
            let p = allow(cmd);
            assert!(approval.contains(&p), "approval.json must allow {p}");
            assert!(!default.contains(&p), "default.json must not allow {p}");
            assert!(!overlay.contains(&p), "overlay.json must not allow {p}");
            assert!(!preview.contains(&p), "preview.json must not allow {p}");
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
    }

    #[test]
    fn every_capability_file_allows_the_stop_command() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut n = 0;
        for entry in std::fs::read_dir(dir).expect("capabilities dir") {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "json") {
                let json = std::fs::read_to_string(&path).expect("read");
                assert!(
                    permissions(&json).contains("allow-kill-agent"),
                    "{path:?} must allow kill_agent"
                );
                n += 1;
            }
        }
        assert!(n >= 4, "capability files found: {n}");
    }

    #[test]
    fn the_desktop_manifest_enables_no_test_doubles() {
        let manifest = include_str!("../Cargo.toml");
        for banned in ["fakes", "test-support"] {
            assert!(
                !manifest
                    .lines()
                    .any(|l| l.contains(banned) && !l.trim_start().starts_with('#')),
                "desktop Cargo.toml mentions {banned}"
            );
        }
    }
}
