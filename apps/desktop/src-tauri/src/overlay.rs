//! One full-screen, transparent, click-through, never-focused overlay window per display.
//!
//! Capture exclusion: Windows calls `exclude_window_from_capture` (plus tauri's
//! `content_protected`) for every overlay; if that fails the overlay is destroyed (fail
//! closed). On macOS the capture crate excludes this whole process by pid; `content_protected`
//! (NSWindowSharingNone) is set as well.
//!
//! Units: the capture crate reports global points on macOS (logical) and physical pixels on
//! Windows. The overlay page converts with `innerWidth / display.w`, so it needs no DPI maths.

use buddy_core::api::DisplayDto;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const PREFIX: &str = "overlay-";

/// Hide `w` from screen capture. `false` = could not (caller must not show it).
pub fn protect(w: &WebviewWindow) -> bool {
    #[cfg(target_os = "windows")]
    {
        match w.hwnd() {
            Ok(h) => buddy_core::exclude_window_from_capture(h.0 as isize).is_ok(),
            Err(_) => false,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = w;
        true
    }
}

/// Close every overlay and create one per display. Call on the main thread.
pub fn rebuild(app: &AppHandle, displays: &[DisplayDto], generation: u32) {
    for (label, w) in app.webview_windows() {
        if label.starts_with(PREFIX) {
            let _ = w.destroy();
        }
    }
    for d in displays {
        if create(app, d, generation).is_err() {
            eprintln!("hey_buddy: could not create an overlay window");
        }
    }
}

fn create(app: &AppHandle, d: &DisplayDto, generation: u32) -> tauri::Result<()> {
    let label = format!("{PREFIX}{}-{generation}", d.id);
    let url = format!(
        "overlay.html?id={}&x={}&y={}&w={}&h={}",
        d.id, d.x, d.y, d.w, d.h
    );
    let w = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        .title("hey_buddy overlay")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .focusable(false)
        .content_protected(true)
        .visible_on_all_workspaces(true)
        .visible(false)
        .build()?;
    let ready = (|| -> tauri::Result<bool> {
        w.set_ignore_cursor_events(true)?;
        place(&w, d)?;
        Ok(protect(&w))
    })();
    match ready {
        Ok(true) => w.show(),
        _ => {
            let _ = w.destroy();
            Ok(())
        }
    }
}

fn place(w: &WebviewWindow, d: &DisplayDto) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use tauri::{LogicalPosition, LogicalSize};
        w.set_position(LogicalPosition::new(d.x as f64, d.y as f64))?;
        w.set_size(LogicalSize::new(d.w as f64, d.h as f64))
    }
    #[cfg(not(target_os = "macos"))]
    {
        use tauri::{PhysicalPosition, PhysicalSize};
        w.set_position(PhysicalPosition::new(d.x, d.y))?;
        w.set_size(PhysicalSize::new(d.w, d.h))
    }
}
