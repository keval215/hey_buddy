//! `FakeCapture`: a synthetic login-form screen with configurable visible windows. Applies the
//! same deny-list logic (shared pure functions in `policy`) as the real platform impls.

use std::time::Instant;

use async_trait::async_trait;
use frame::{DisplayId, DisplayInfo, Frame, Rect, WindowInfo};

use crate::policy::{filter_windows, windows_policy, Decision};
use crate::{CaptureError, CaptureRequest, CaptureScope, CaptureSource};

pub const WIDTH: u32 = 800;
pub const HEIGHT: u32 = 600;
pub const BACKGROUND: [u8; 4] = [255, 255, 255, 255];
pub const FIELD_COLOR: [u8; 4] = [230, 230, 230, 255];
pub const PASSWORD_COLOR: [u8; 4] = [200, 220, 255, 255];
/// Known password-field rect (pixels == points; scale is 1.0) for later firewall tests.
pub const PASSWORD_FIELD: Rect = Rect::new(300, 300, 200, 30);
pub const USERNAME_FIELD: Rect = Rect::new(300, 240, 200, 30);

/// Which real-platform behaviour the fake imitates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeMode {
    /// macOS: deny-listed windows are excluded from the image.
    ExcludeWindows,
    /// Windows: refuse if a deny-listed window is visible; foreground fallback.
    WindowsPolicy,
}

#[derive(Debug, Clone)]
pub struct FakeWindow {
    pub info: WindowInfo,
    /// Solid colour the window is painted with (use colours distinct from the form).
    pub color: [u8; 4],
}

#[derive(Debug, Clone)]
pub struct FakeCapture {
    pub display: DisplayInfo,
    pub mode: FakeMode,
    /// Painted in order; later windows are on top.
    pub windows: Vec<FakeWindow>,
    /// Index into `windows` of the foreground window.
    pub foreground: Option<usize>,
    /// If set, every capture fails with this error.
    pub fail_with: Option<CaptureError>,
}

impl FakeCapture {
    pub fn new(mode: FakeMode) -> Self {
        Self {
            display: DisplayInfo {
                id: DisplayId(1),
                origin: (0, 0),
                size: (WIDTH, HEIGHT),
                scale: 1.0,
            },
            mode,
            windows: Vec::new(),
            foreground: None,
            fail_with: None,
        }
    }

    pub fn with_window(mut self, app_id: &str, rect: Rect, color: [u8; 4]) -> Self {
        let pid = 100 + self.windows.len() as u32;
        self.windows.push(FakeWindow {
            info: WindowInfo {
                app_id: app_id.into(),
                pid,
                rect,
            },
            color,
        });
        self
    }

    pub fn with_foreground(mut self, index: usize) -> Self {
        self.foreground = Some(index);
        self
    }

    fn base_pixels(w: u32, h: u32) -> Vec<u8> {
        let mut px = BACKGROUND.repeat((w * h) as usize);
        fill(&mut px, w, h, USERNAME_FIELD, FIELD_COLOR);
        fill(&mut px, w, h, PASSWORD_FIELD, PASSWORD_COLOR);
        px
    }

    fn make_frame(
        &self,
        pixels: Vec<u8>,
        w: u32,
        h: u32,
        excluded: Vec<WindowInfo>,
    ) -> Result<Frame, CaptureError> {
        Frame::new(pixels, w, h, w * 4, self.display, Instant::now(), excluded)
            .map_err(|_| CaptureError::Platform("invalid frame".into()))
    }
}

fn fill(px: &mut [u8], w: u32, h: u32, r: Rect, color: [u8; 4]) {
    let Some(r) = r.clamp_to(&Rect::new(0, 0, w, h)) else {
        return;
    };
    for y in r.y as u32..r.y as u32 + r.h {
        for x in r.x as u32..r.x as u32 + r.w {
            let i = ((y * w + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&color);
        }
    }
}

#[async_trait]
impl CaptureSource for FakeCapture {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        Ok(vec![self.display])
    }

    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError> {
        if let Some(e) = &self.fail_with {
            return Err(e.clone());
        }
        if req.display != self.display.id {
            return Err(CaptureError::Platform("unknown display".into()));
        }
        let infos: Vec<WindowInfo> = self.windows.iter().map(|w| w.info.clone()).collect();
        let color_of = |info: &WindowInfo| {
            self.windows
                .iter()
                .find(|w| w.info == *info)
                .map(|w| w.color)
                .unwrap_or([0, 0, 0, 255])
        };

        match self.mode {
            FakeMode::ExcludeWindows => {
                let (kept, excluded) = filter_windows(&infos, &req.deny_list);
                let mut px = Self::base_pixels(WIDTH, HEIGHT);
                match req.scope {
                    CaptureScope::Display => {
                        for w in &kept {
                            fill(&mut px, WIDTH, HEIGHT, w.rect, color_of(w));
                        }
                        self.make_frame(px, WIDTH, HEIGHT, excluded)
                    }
                    CaptureScope::ForegroundWindow => {
                        let fg = self.foreground.and_then(|i| infos.get(i));
                        match fg {
                            Some(w) if !req.deny_list.matches(&w.app_id) => {
                                self.window_frame(w, color_of(w), excluded)
                            }
                            Some(w) => Err(CaptureError::DeniedAppVisible {
                                app: w.app_id.clone(),
                            }),
                            None => Err(CaptureError::Platform("no foreground window".into())),
                        }
                    }
                }
            }
            FakeMode::WindowsPolicy => {
                let fg = self.foreground.and_then(|i| infos.get(i));
                match windows_policy(&infos, req.scope, fg, &req.deny_list) {
                    Decision::Refuse(e) => Err(e),
                    Decision::CaptureDisplay => {
                        let mut px = Self::base_pixels(WIDTH, HEIGHT);
                        for w in &infos {
                            fill(&mut px, WIDTH, HEIGHT, w.rect, color_of(w));
                        }
                        self.make_frame(px, WIDTH, HEIGHT, vec![])
                    }
                    Decision::CaptureForeground(w) => {
                        let (_, excluded) = filter_windows(&infos, &req.deny_list);
                        self.window_frame(&w, color_of(&w), excluded)
                    }
                }
            }
        }
    }
}

impl FakeCapture {
    /// Frame containing only one window (its rect clamped to the display).
    fn window_frame(
        &self,
        w: &WindowInfo,
        color: [u8; 4],
        excluded: Vec<WindowInfo>,
    ) -> Result<Frame, CaptureError> {
        let r = w
            .rect
            .clamp_to(&self.display.bounds())
            .ok_or_else(|| CaptureError::Platform("window off display".into()))?;
        let mut px = color.repeat((r.w * r.h) as usize);
        // Keep layout trivially valid; contents are the window colour only.
        px.shrink_to_fit();
        self.make_frame(px, r.w, r.h, excluded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DenyList;
    use futures::executor::block_on;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    fn req(scope: CaptureScope) -> CaptureRequest {
        CaptureRequest {
            display: DisplayId(1),
            deny_list: DenyList::defaults(),
            scope,
        }
    }

    fn has_color(f: &Frame, c: [u8; 4]) -> bool {
        f.pixels().chunks_exact(4).any(|p| p == c)
    }

    fn setup(mode: FakeMode) -> FakeCapture {
        FakeCapture::new(mode)
            .with_window("com.1password.1password", Rect::new(20, 20, 150, 150), RED)
            .with_window("com.apple.finder", Rect::new(400, 20, 150, 150), GREEN)
    }

    #[test]
    fn base_frame_has_password_field() {
        let f = block_on(
            FakeCapture::new(FakeMode::ExcludeWindows).capture(&req(CaptureScope::Display)),
        )
        .unwrap();
        let i = ((PASSWORD_FIELD.y as u32 * WIDTH + PASSWORD_FIELD.x as u32) * 4) as usize;
        assert_eq!(&f.pixels()[i..i + 4], &PASSWORD_COLOR);
    }

    #[test]
    fn denied_window_pixels_absent_macos_mode() {
        let f =
            block_on(setup(FakeMode::ExcludeWindows).capture(&req(CaptureScope::Display))).unwrap();
        assert!(!has_color(&f, RED));
        assert!(has_color(&f, GREEN));
        assert_eq!(f.excluded_windows().len(), 1);
        assert_eq!(f.excluded_windows()[0].app_id, "com.1password.1password");
    }

    #[test]
    fn user_added_entry_is_excluded() {
        let mut r = req(CaptureScope::Display);
        r.deny_list = DenyList::from_config(&crate::deny::UserConfig {
            add: vec!["COM.APPLE.FINDER".into()],
            remove: vec![],
        });
        let f = block_on(setup(FakeMode::ExcludeWindows).capture(&r)).unwrap();
        assert!(!has_color(&f, GREEN) && !has_color(&f, RED));
    }

    #[test]
    fn windows_mode_refuses_when_denied_visible() {
        let e = block_on(setup(FakeMode::WindowsPolicy).capture(&req(CaptureScope::Display)))
            .unwrap_err();
        assert_eq!(
            e,
            CaptureError::DeniedAppVisible {
                app: "com.1password.1password".into()
            }
        );
    }

    #[test]
    fn windows_mode_foreground_fallback() {
        let f = block_on(
            setup(FakeMode::WindowsPolicy)
                .with_foreground(1)
                .capture(&req(CaptureScope::ForegroundWindow)),
        )
        .unwrap();
        assert_eq!((f.width(), f.height()), (150, 150));
        assert!(has_color(&f, GREEN) && !has_color(&f, RED));
        assert_eq!(f.excluded_windows().len(), 1);
    }

    #[test]
    fn windows_mode_foreground_denied_errors() {
        let e = block_on(
            setup(FakeMode::WindowsPolicy)
                .with_foreground(0)
                .capture(&req(CaptureScope::ForegroundWindow)),
        )
        .unwrap_err();
        assert!(matches!(e, CaptureError::DeniedAppVisible { .. }));
        let e = block_on(
            setup(FakeMode::ExcludeWindows)
                .with_foreground(0)
                .capture(&req(CaptureScope::ForegroundWindow)),
        )
        .unwrap_err();
        assert!(matches!(e, CaptureError::DeniedAppVisible { .. }));
    }

    #[test]
    fn windows_mode_no_denied_captures_display() {
        let c = FakeCapture::new(FakeMode::WindowsPolicy).with_window(
            "notepad.exe",
            Rect::new(0, 0, 50, 50),
            GREEN,
        );
        let f = block_on(c.capture(&req(CaptureScope::Display))).unwrap();
        assert_eq!((f.width(), f.height()), (WIDTH, HEIGHT));
    }

    #[test]
    fn errors_never_return_a_frame() {
        let mut c = setup(FakeMode::ExcludeWindows);
        c.fail_with = Some(CaptureError::PermissionDenied);
        assert_eq!(
            block_on(c.capture(&req(CaptureScope::Display))).unwrap_err(),
            CaptureError::PermissionDenied
        );
        let mut r = req(CaptureScope::Display);
        r.display = DisplayId(99);
        assert!(block_on(setup(FakeMode::ExcludeWindows).capture(&r)).is_err());
    }

    #[test]
    fn error_text_has_no_titles() {
        // WindowInfo has no title field, so errors cannot carry one; check the strings anyway.
        let c = setup(FakeMode::WindowsPolicy);
        let e = block_on(c.capture(&req(CaptureScope::Display))).unwrap_err();
        let s = format!("{e} {e:?}");
        assert!(!s.to_lowercase().contains("title"));
    }
}
