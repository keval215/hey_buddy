//! The approval window (S2-16, ADR 0003 section 7).
//!
//! A separate, always-on-top window that stays hidden until core's `ApprovalBroker` asks for a
//! plan or step approval. It is excluded from screen capture like the other windows (Windows:
//! `overlay::protect` = `exclude_window_from_capture`, all platforms: `content_protected`); if
//! exclusion fails the window is destroyed, so no approval can be shown and every request is
//! denied (fail closed). Closing the window denies everything that is pending.
//!
//! The window only receives display DTOs and can only answer through the three commands in
//! `capabilities/approval.json` (plus the stop-only `kill_agent`). It can never create an approval.

use buddy_core::approval::{
    ApprovalBroker, ApprovalClosedDto, ApprovalRequestDto, ApprovalUi, APPROVAL_WINDOW,
    EVT_APPROVAL_CLOSED, EVT_APPROVAL_REQUEST,
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::overlay;

/// Create the (hidden) approval window and attach it to the broker as its display.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let w = WebviewWindowBuilder::new(
        app,
        APPROVAL_WINDOW,
        WebviewUrl::App("approval.html".into()),
    )
    .title("hey_buddy: approval needed")
    .inner_size(560.0, 640.0)
    .min_inner_size(420.0, 360.0)
    .resizable(true)
    .always_on_top(true)
    .content_protected(true)
    .center()
    .visible(false)
    .build()?;
    if !overlay::protect(&w) {
        let _ = w.destroy();
        eprintln!("hey_buddy: could not hide the approval window from capture; agent approvals are denied");
        return Ok(());
    }
    let app2 = app.clone();
    let w2 = w.clone();
    w.on_window_event(move |e| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            let _ = w2.hide();
            // Closing is "no": whatever is pending is denied.
            if let Some(b) = app2.try_state::<ApprovalBroker>() {
                b.deny_all();
            }
        }
    });
    if let Some(b) = app.try_state::<ApprovalBroker>() {
        b.set_ui(std::sync::Arc::new(TauriApprovalUi(app.clone())));
    }
    Ok(())
}

/// Shows approvals in the approval window.
pub struct TauriApprovalUi(pub AppHandle);

impl ApprovalUi for TauriApprovalUi {
    fn show(&self, request: &ApprovalRequestDto) -> bool {
        let Some(w) = self.0.get_webview_window(APPROVAL_WINDOW) else {
            return false;
        };
        if self
            .0
            .emit_to(APPROVAL_WINDOW, EVT_APPROVAL_REQUEST, request.clone())
            .is_err()
        {
            return false;
        }
        if w.show().is_err() {
            return false;
        }
        // Focus so Esc and Ctrl+Shift+A reach the window. Nothing inside is focused: no button
        // can be triggered by Enter or Space (the page ignores them, and the 500 ms guard holds).
        let _ = w.set_focus();
        true
    }

    fn close(&self, closed: &ApprovalClosedDto) {
        let _ = self
            .0
            .emit_to(APPROVAL_WINDOW, EVT_APPROVAL_CLOSED, closed.clone());
        let none_left = self
            .0
            .try_state::<ApprovalBroker>()
            .is_none_or(|b| b.pending_count() == 0);
        if none_left {
            if let Some(w) = self.0.get_webview_window(APPROVAL_WINDOW) {
                let _ = w.hide();
            }
        }
    }
}
