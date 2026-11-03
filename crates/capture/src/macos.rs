//! macOS capture via ScreenCaptureKit (`screencapturekit` crate, MIT OR Apache-2.0).
//!
//! Deny-listed applications and our own process are excluded in the `SCContentFilter`, so their
//! windows are never rendered into the image. UNTESTED on this machine (needs macOS 14+ and
//! Screen Recording permission). `CaptureScope::ForegroundWindow` is treated like `Display`
//! because exclusion works here.
//!
//! Known limitations: windows/apps without a bundle id cannot be matched against the
//! deny-list; only our own main process is excluded (not helper processes); any
//! `SCShareableContent` failure is reported as `PermissionDenied`.
//!
//! The capture call blocks the calling thread; callers should run it on a blocking thread.

use std::time::Instant;

use async_trait::async_trait;
use frame::{DisplayId, DisplayInfo, Frame, Rect, WindowInfo};
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::{CGImageExt, SCScreenshotManager};

use crate::policy::filter_windows;
use crate::{CaptureError, CaptureRequest, CaptureSource, DenyList};

pub(crate) struct MacCapture;

impl MacCapture {
    pub(crate) fn new() -> Self {
        Self
    }
}

// Fixed strings only: SCError text is never forwarded (it could echo app or window names).
fn platform(msg: &str) -> CaptureError {
    CaptureError::Platform(msg.into())
}

fn shareable() -> Result<SCShareableContent, CaptureError> {
    // Failure here is almost always a missing Screen Recording permission.
    SCShareableContent::get().map_err(|_| CaptureError::PermissionDenied)
}

fn rect_of(r: CGRect) -> Rect {
    Rect::new(
        r.origin.x.floor() as i32,
        r.origin.y.floor() as i32,
        r.size.width.ceil() as u32,
        r.size.height.ceil() as u32,
    )
}

fn display_info(d: &SCDisplay) -> Result<DisplayInfo, CaptureError> {
    let filter = SCContentFilter::create()
        .with_display(d)
        .build()
        .map_err(|_| platform("filter"))?;
    let f = rect_of(d.frame());
    Ok(DisplayInfo {
        id: DisplayId(d.display_id()),
        origin: (f.x, f.y),
        size: (f.w, f.h),
        scale: filter.point_pixel_scale(),
    })
}

/// Apps to exclude: deny-listed ones and our own process.
fn excluded_apps(content: &SCShareableContent, deny: &DenyList) -> Vec<SCRunningApplication> {
    let own = std::process::id() as i32;
    content
        .applications()
        .into_iter()
        .filter(|a| a.process_id() == own || deny.matches(&a.bundle_identifier()))
        .collect()
}

/// Denied windows currently on screen on this display (titles are not read).
fn denied_windows(content: &SCShareableContent, deny: &DenyList, disp: &Rect) -> Vec<WindowInfo> {
    let all: Vec<WindowInfo> = content
        .windows()
        .into_iter()
        .filter(|w| w.is_on_screen())
        .filter_map(|w| {
            let app = w.owning_application()?;
            let rect = rect_of(w.frame());
            rect.intersects(disp).then(|| WindowInfo {
                app_id: app.bundle_identifier(),
                pid: app.process_id().max(0) as u32,
                rect,
            })
        })
        .collect();
    filter_windows(&all, deny).1
}

fn capture_blocking(req: &CaptureRequest) -> Result<Frame, CaptureError> {
    let content = shareable()?;
    let display = content
        .displays()
        .into_iter()
        .find(|d| d.display_id() == req.display.0)
        .ok_or_else(|| platform("unknown display"))?;
    let info = display_info(&display)?;
    let apps = excluded_apps(&content, &req.deny_list);
    let before = denied_windows(&content, &req.deny_list, &info.bounds());

    let refs: Vec<&SCRunningApplication> = apps.iter().collect();
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_applications(&refs, &[])
        .build()
        .map_err(|_| platform("filter"))?;
    let (pw, ph) = info.pixel_size();
    let config = SCStreamConfiguration::new()
        .with_width(pw)
        .with_height(ph)
        .with_shows_cursor(false);

    let image = SCScreenshotManager::capture_image(&filter, &config)
        .map_err(|_| platform("capture failed"))?;
    let (w, h) = (image.width() as u32, image.height() as u32);
    let pixels = image
        .rgba_data()
        .map_err(|_| platform("pixel conversion failed"))?;

    // Fail closed on a race: a deny-listed app that appeared after we built the exclusion list
    // would not be excluded from this image.
    let after_content = shareable()?;
    let after = denied_windows(&after_content, &req.deny_list, &info.bounds());
    let known: Vec<u32> = before.iter().map(|w| w.pid).collect();
    if let Some(w) = after.iter().find(|w| !known.contains(&w.pid)) {
        return Err(CaptureError::DeniedAppVisible {
            app: w.app_id.clone(),
        });
    }

    Frame::new(pixels, w, h, w * 4, info, Instant::now(), before)
        .map_err(|_| platform("invalid frame"))
}

#[async_trait]
impl CaptureSource for MacCapture {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        shareable()?.displays().iter().map(display_info).collect()
    }

    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError> {
        capture_blocking(req)
    }
}
