use tauri_build::{AppManifest, Attributes, WindowsAttributes};

/// App commands. Each gets an `allow-<name>` permission (see capabilities/); a window without
/// that permission cannot call the command, so the overlay webview can call none of them.
const COMMANDS: &[&str] = &[
    "get_status",
    "get_settings",
    "update_settings",
    "list_displays",
    "cancel_turn",
    "debug_point",
    "preview_decision",
    "get_last_request",
    "set_api_key",
    "clear_api_key",
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
