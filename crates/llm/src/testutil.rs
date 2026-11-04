//! Test helpers: real `RedactedFrame` / `RedactedScreenText` values produced by the real
//! `PrivacyFirewall` with fake detector / probe / text source (the only way to obtain them).

use std::time::Instant;

use firewall::fake::{FakeDetector, FakeSessionProbe, FakeTextSource};
use firewall::{
    Firewall, NodeRole, PrivacyFirewall, RawTextNode, RawWindowTree, RedactedFrame,
    RedactedScreenText, SessionState, TextVerdict, Verdict,
};
use frame::{AppDenyPolicy, DisplayId, DisplayInfo, Frame, Rect};

pub(crate) const MARKER: &str = "PRIVATE_MARKER_123";

pub(crate) fn display(w: u32, h: u32) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId(7),
        origin: (-1920, 40),
        size: (w, h),
        scale: 1.0,
    }
}

/// Noisy (poorly compressible, deterministic) background so PNG size tracks pixel count.
fn noisy_pixels(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    let mut s: u32 = 0x1234_5678;
    for _ in 0..(w * h) {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let b = (s >> 24) as u8 | 0x40; // never pure black
        v.extend_from_slice(&[b, b.wrapping_add(31) | 0x40, b.wrapping_add(77) | 0x40, 255]);
    }
    v
}

/// A Clear `RedactedFrame` of `w x h`; `secure` are detector rects (global == pixel space here
/// after the display origin shift is applied by the caller; we pass frame-local rects shifted by
/// the origin).
pub(crate) fn clear_frame_with(w: u32, h: u32, secure: Vec<Rect>) -> RedactedFrame {
    let d = display(w, h);
    let global: Vec<Rect> = secure
        .into_iter()
        .map(|r| Rect::new(r.x + d.origin.0, r.y + d.origin.1, r.w, r.h))
        .collect();
    let fw = PrivacyFirewall::new(FakeDetector::new(global), FakeSessionProbe::normal());
    assert_eq!(fw.preflight(), SessionState::Active);
    let f = Frame::new(noisy_pixels(w, h), w, h, w * 4, d, Instant::now(), vec![]).unwrap();
    match fw.check(f) {
        Verdict::Clear(rf) => rf,
        other => panic!("expected Clear, got {other:?}"),
    }
}

pub(crate) fn clear_frame(w: u32, h: u32) -> RedactedFrame {
    clear_frame_with(w, h, vec![])
}

struct DenyNone;
impl AppDenyPolicy for DenyNone {
    fn is_denied(&self, _: &str) -> bool {
        false
    }
}

fn node(depth: u16, role: NodeRole, label: Option<&str>, value: Option<&str>) -> RawTextNode {
    RawTextNode {
        depth,
        role,
        secure: false,
        editable: false,
        label: label.map(String::from),
        value: value.map(String::from),
        has_value: false,
        rect: Some(Rect::new(110, 110, 300, 40)),
    }
}

/// Screen text containing a button and a static text whose value is `static_value`
/// (emitted verbatim by the firewall), plus a hidden password field.
pub(crate) fn screen_text_with(static_value: &str) -> RedactedScreenText {
    let win = Rect::new(100, 100, 800, 600);
    let pw = Rect::new(200, 260, 300, 30);
    let tree = RawWindowTree {
        app_id: "com.example.app".into(),
        window_rect: win,
        nodes: vec![
            RawTextNode {
                rect: Some(win),
                ..node(0, NodeRole::Window, Some("TITLE"), None)
            },
            node(1, NodeRole::StaticText, None, Some(static_value)),
            node(1, NodeRole::Button, Some("Sign in"), None),
            RawTextNode {
                secure: true,
                editable: true,
                rect: Some(pw),
                ..node(1, NodeRole::TextField, Some("Password"), None)
            },
        ],
    };
    let fw = PrivacyFirewall::new(FakeDetector::new(vec![pw]), FakeSessionProbe::normal())
        .with_text_source(FakeTextSource::new(tree));
    assert_eq!(fw.preflight(), SessionState::Active);
    match fw.check_text(&DenyNone) {
        TextVerdict::Clear(t) => t,
        other => panic!("expected Clear, got {other:?}"),
    }
}
