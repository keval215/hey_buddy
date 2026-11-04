//! Windows capture via Windows Graphics Capture (`windows` crate, MIT OR Apache-2.0).
//!
//! WGC cannot exclude windows from a monitor capture, so (ADR 0001) a full-display capture is
//! refused with `DeniedAppVisible` whenever a deny-listed window is visible on that display.
//! `CaptureScope::ForegroundWindow` captures only the foreground window if it is not
//! deny-listed. Our own windows must be hidden from capture with
//! [`exclude_window_from_capture`] (WDA_EXCLUDEFROMCAPTURE); their pid is also skipped in the
//! visible-window scan. Coordinates are physical pixels (the app is per-monitor DPI aware),
//! so `DisplayInfo.scale` is 1.0.
//!
//! Known limitations and requirements:
//! - UWP/packaged apps host their windows in `ApplicationFrameHost.exe`, so exe-name
//!   deny-listing cannot match UWP windows. Known limitation.
//! - core must capture BEFORE the overlay takes focus (otherwise the foreground window is ours).
//! - The desktop app MUST call [`exclude_window_from_capture`] for every overlay/panel window;
//!   only our process id is skipped in the scan, and nothing else hides our windows in frames.
//!
//! UNTESTED on this machine (only cross-checked with `cargo check`). Blocks the calling thread.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use frame::{DisplayId, DisplayInfo, Frame, Rect, WindowInfo};
use windows::core::{Interface, BOOL};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{CloseHandle, HMODULE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
};

use crate::policy::{windows_policy, Decision};
use crate::{CaptureError, CaptureRequest, CaptureScope, CaptureSource};

const TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) struct WindowsCapture;

impl WindowsCapture {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// Hide one of our own windows (overlay, panel) from all screen capture.
pub fn exclude_window_from_capture(hwnd: isize) -> Result<(), CaptureError> {
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|_| platform("window exclusion failed"))
}

// Fixed strings only: OS error text is never forwarded.
fn platform(msg: &str) -> CaptureError {
    CaptureError::Platform(msg.into())
}

fn rect_of(r: RECT) -> Rect {
    Rect::new(
        r.left,
        r.top,
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

// ---- displays ----

unsafe extern "system" fn monitor_cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<(HMONITOR, Rect)>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(m, &mut info).as_bool() {
        out.push((m, rect_of(info.rcMonitor)));
    }
    TRUE
}

fn monitors() -> Result<Vec<(HMONITOR, DisplayInfo)>, CaptureError> {
    let mut raw: Vec<(HMONITOR, Rect)> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_cb),
            LPARAM(&mut raw as *mut _ as isize),
        )
    };
    if !ok.as_bool() || raw.is_empty() {
        return Err(platform("no displays"));
    }
    Ok(raw
        .into_iter()
        .map(|(m, r)| {
            let info = DisplayInfo {
                id: DisplayId(m.0 as usize as u32),
                origin: (r.x, r.y),
                size: (r.w, r.h),
                scale: 1.0,
            };
            (m, info)
        })
        .collect())
}

// ---- windows ----

struct Scan {
    out: Vec<WindowInfo>,
    hwnds: Vec<HWND>,
    own_pid: u32,
    bounds: Rect,
    failed: bool,
}

fn exe_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16(&buf[..len as usize]).ok()?;
        path.rsplit(['\\', '/'])
            .next()
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    }
}

fn window_info(hwnd: HWND) -> Option<Result<WindowInfo, ()>> {
    unsafe {
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Unresolvable exe name: report an error so the caller fails closed.
        Some(
            exe_name(pid)
                .map(|app_id| WindowInfo {
                    app_id,
                    pid,
                    rect: rect_of(r),
                })
                .ok_or(()),
        )
    }
}

unsafe extern "system" fn window_cb(hwnd: HWND, data: LPARAM) -> BOOL {
    let scan = &mut *(data.0 as *mut Scan);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return TRUE;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4).is_ok()
        && cloaked != 0
    {
        return TRUE;
    }
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() || !rect_of(r).intersects(&scan.bounds) {
        return TRUE;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == scan.own_pid {
        return TRUE;
    }
    match window_info(hwnd) {
        Some(Ok(w)) => {
            scan.out.push(w);
            scan.hwnds.push(hwnd);
        }
        _ => scan.failed = true,
    }
    TRUE
}

/// Visible windows intersecting `bounds`, excluding our own process. Errors if any visible
/// window's app cannot be identified (fail closed).
fn visible_windows(bounds: Rect) -> Result<(Vec<WindowInfo>, Vec<HWND>), CaptureError> {
    let mut scan = Scan {
        out: vec![],
        hwnds: vec![],
        own_pid: std::process::id(),
        bounds,
        failed: false,
    };
    unsafe { EnumWindows(Some(window_cb), LPARAM(&mut scan as *mut _ as isize)) }
        .map_err(|_| platform("window enumeration failed"))?;
    if scan.failed {
        return Err(platform("window enumeration failed"));
    }
    Ok((scan.out, scan.hwnds))
}

// ---- WGC ----

struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

fn gpu() -> Result<Gpu, CaptureError> {
    let e = |_| platform("graphics device failed");
    unsafe {
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(e)?;
        let device = device.ok_or_else(|| platform("graphics device failed"))?;
        let ctx = ctx.ok_or_else(|| platform("graphics device failed"))?;
        let dxgi: IDXGIDevice = device.cast().map_err(e)?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
            .map_err(e)?
            .cast()
            .map_err(e)?;
        Ok(Gpu { device, ctx, winrt })
    }
}

/// Grab one frame of `item` as RGBA8: (pixels, width, height).
fn grab(item: &GraphicsCaptureItem) -> Result<(Vec<u8>, u32, u32), CaptureError> {
    let e = |_| platform("capture failed");
    let g = gpu()?;
    let size = item.Size().map_err(e)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &g.winrt,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        size,
    )
    .map_err(e)?;
    let session = pool.CreateCaptureSession(item).map_err(e)?;
    let _ = session.SetIsCursorCaptureEnabled(false);
    session.StartCapture().map_err(e)?;

    let start = Instant::now();
    let result = loop {
        if let Ok(f) = pool.TryGetNextFrame() {
            break Ok(f);
        }
        if start.elapsed() > TIMEOUT {
            break Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = result.and_then(|f| {
        let surface = f.Surface().map_err(e)?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(e)?;
        let tex: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(e)?;
        read_texture(&g, &tex)
    });
    let _ = session.Close();
    let _ = pool.Close();
    out
}

fn read_texture(g: &Gpu, tex: &ID3D11Texture2D) -> Result<(Vec<u8>, u32, u32), CaptureError> {
    let e = |_| platform("capture failed");
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        g.device
            .CreateTexture2D(&desc, None, Some(&mut staging))
            .map_err(e)?;
        let staging = staging.ok_or_else(|| platform("capture failed"))?;
        g.ctx.CopyResource(&staging, tex);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        g.ctx
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
            .map_err(e)?;
        let (w, h) = (desc.Width, desc.Height);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts(
                (m.pData as *const u8).add(y * m.RowPitch as usize),
                (w * 4) as usize,
            );
            for px in row.chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], 255]); // BGRA -> RGBA
            }
        }
        g.ctx.Unmap(&staging, 0);
        Ok((out, w, h))
    }
}

fn capture_blocking(req: &CaptureRequest) -> Result<Frame, CaptureError> {
    let (hmon, info) = monitors()?
        .into_iter()
        .find(|(_, d)| d.id == req.display)
        .ok_or_else(|| platform("unknown display"))?;
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|_| CaptureError::Unsupported)?;

    let (visible, hwnds) = visible_windows(info.bounds())?;
    let fg_hwnd = unsafe { GetForegroundWindow() };
    let fg = hwnds
        .iter()
        .position(|h| *h == fg_hwnd)
        .map(|i| visible[i].clone());

    let decision = windows_policy(&visible, req.scope, fg.as_ref(), &req.deny_list);
    let (item, frame_display, excluded): (GraphicsCaptureItem, DisplayInfo, Vec<WindowInfo>) =
        match decision {
            Decision::Refuse(e) => return Err(e),
            Decision::CaptureDisplay => {
                let item = unsafe { interop.CreateForMonitor(hmon) }
                    .map_err(|_| platform("capture failed"))?;
                (item, info, vec![])
            }
            Decision::CaptureForeground(_) => {
                let item = unsafe { interop.CreateForWindow(fg_hwnd) }
                    .map_err(|_| platform("capture failed"))?;
                let (_, excluded) = crate::policy::filter_windows(&visible, &req.deny_list);
                (item, info, excluded)
            }
        };

    let (pixels, w, h) = grab(&item)?;

    // Foreground scope: the foreground window must be the same one and still not deny-listed.
    if req.scope == CaptureScope::ForegroundWindow {
        let now = unsafe { GetForegroundWindow() };
        if now != fg_hwnd {
            return Err(platform("foreground window changed"));
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(now, Some(&mut pid)) };
        match exe_name(pid) {
            Some(app) if !req.deny_list.matches(&app) => {}
            Some(app) => return Err(CaptureError::DeniedAppVisible { app }),
            None => return Err(platform("foreground window changed")),
        }
    }

    // Fail closed on a race: a deny-listed window that appeared during capture.
    if req.scope == CaptureScope::Display {
        let (after, _) = visible_windows(info.bounds())?;
        if let Some(w) = after.iter().find(|w| req.deny_list.matches(&w.app_id)) {
            return Err(CaptureError::DeniedAppVisible {
                app: w.app_id.clone(),
            });
        }
    }
    Frame::new(pixels, w, h, w * 4, frame_display, Instant::now(), excluded)
        .map_err(|_| platform("invalid frame"))
}

#[async_trait]
impl CaptureSource for WindowsCapture {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        Ok(monitors()?.into_iter().map(|(_, d)| d).collect())
    }

    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError> {
        capture_blocking(req)
    }
}
