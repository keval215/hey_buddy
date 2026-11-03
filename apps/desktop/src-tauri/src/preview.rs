//! The preview panel window and the `preview://` URI scheme (T11, ADR 0002 A3).
//!
//! The panel is a normal, focusable, always-on-top window that stays hidden until the core
//! asks for a cloud preview. It is excluded from screen capture like the other windows
//! (Windows: `exclude_window_from_capture`, all platforms: `content_protected`); if exclusion
//! fails the window is destroyed, so no preview can be shown and every cloud request is
//! cancelled (fail closed). Closing the window cancels the pending request.
//!
//! `preview://<id>` serves the request's PNG and `preview://<id>/body` its exact JSON bytes
//! from the core's in-memory store, `no-store`, only to the windows allowed by
//! `buddy_core::consent::window_may_load`. Windows serves custom schemes as
//! `http://preview.localhost/<id>[/body]`; both shapes are parsed by the core.

use std::sync::Arc;

use buddy_core::api::{PreviewClosed, PreviewDto, EVT_PREVIEW_CLOSED, EVT_TURN_PREVIEW};
use buddy_core::consent::{parse_uri, window_may_load, PreviewSink};
use buddy_core::Buddy;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, UriSchemeContext, WebviewUrl, WebviewWindowBuilder, Wry};

use crate::overlay;

pub const LABEL: &str = "preview";
pub const SCHEME: &str = "preview";

/// Create the (hidden) preview window.
pub fn create(app: &AppHandle, buddy: &Arc<Buddy>) -> tauri::Result<()> {
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("preview.html".into()))
        .title("hey_buddy: review before sending")
        .inner_size(640.0, 760.0)
        .min_inner_size(480.0, 520.0)
        .resizable(true)
        .always_on_top(true)
        .content_protected(true)
        .center()
        .visible(false)
        .build()?;
    if !overlay::protect(&w) {
        let _ = w.destroy();
        eprintln!("hey_buddy: could not hide the preview window from capture; cloud disabled");
        return Ok(());
    }
    let (w2, buddy) = (w.clone(), buddy.clone());
    w.on_window_event(move |e| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            let _ = w2.hide();
            buddy.cancel_pending_consents();
        }
    });
    Ok(())
}

/// Shows the panel for the core's consent gate.
pub struct TauriSink(pub AppHandle);

impl PreviewSink for TauriSink {
    fn show(&self, dto: PreviewDto) -> bool {
        let Some(w) = self.0.get_webview_window(LABEL) else {
            return false;
        };
        if self.0.emit_to(LABEL, EVT_TURN_PREVIEW, dto).is_err() {
            return false;
        }
        if w.show().is_err() {
            return false;
        }
        let _ = w.set_focus();
        true
    }

    fn close(&self, request_id: u64) {
        let _ = self
            .0
            .emit_to(LABEL, EVT_PREVIEW_CLOSED, PreviewClosed { request_id });
        if let Some(w) = self.0.get_webview_window(LABEL) {
            let _ = w.hide();
        }
    }
}

fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        // The page fetches the raw body cross-origin; the label check above is the access control.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// Handler of the `preview` scheme.
pub fn handle(ctx: UriSchemeContext<'_, Wry>, req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let not_found = || respond(StatusCode::NOT_FOUND, "text/plain", Vec::new());
    let Some(buddy) = ctx.app_handle().try_state::<Arc<Buddy>>() else {
        return not_found();
    };
    let uri = req.uri();
    let Some((key, kind)) = parse_uri(uri.host(), uri.path()) else {
        return not_found();
    };
    if !window_may_load(ctx.webview_label(), &key) {
        return not_found();
    }
    match buddy.preview_store().asset(&key, kind) {
        Some(a) => respond(StatusCode::OK, a.content_type, a.bytes.to_vec()),
        None => not_found(),
    }
}
