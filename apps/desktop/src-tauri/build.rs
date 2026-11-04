use tauri_build::{AppManifest, Attributes, WindowsAttributes};

/// App commands. Each gets an `allow-<name>` permission (see capabilities/); a window without
/// that permission cannot call the command, so the overlay webview can call none of them.
const COMMANDS: &[&str] = &[
    "get_status",
    "get_settings",
    "update_settings",
    "request_engine_swap",
    "list_displays",
    "cancel_turn",
    "debug_point",
    "preview_decision",
    "get_last_request",
    "get_pending_preview",
    "set_api_key",
    "clear_api_key",
    "list_models",
    "start_download",
    "cancel_download",
    "get_deny_list",
    "add_deny_entry",
    "remove_deny_entry",
    "reset_deny_list_to_defaults",
    "get_audit_entries",
    "export_audit_log",
    "clear_audit_log",
    "kill_agent",
    "approval_ready",
    "approval_displayed",
    "approval_respond",
    "debug_fake_task",
];

fn main() {
    let attrs = Attributes::new()
        .app_manifest(AppManifest::new().commands(COMMANDS))
        // Replaces tauri's default manifest with one that adds PerMonitorV2 DPI awareness.
        .windows_attributes(
            WindowsAttributes::new().app_manifest(include_str!("windows-app-manifest.xml")),
        );
    tauri_build::try_build(attrs).expect("tauri build failed");
}
