//! Pure blackout: fill padded, clamped rectangles with opaque black.

use frame::{DisplayInfo, Frame, Rect};

pub(crate) const DEFAULT_PADDING_PX: u32 = 4;

/// Convert a rect in global points to frame pixels: subtract the display origin, apply the
/// scale (rounding outward), pad, and clamp to `(0,0,w,h)`. `None` when nothing remains or the
/// scale is unusable. Overflow-safe.
pub(crate) fn to_pixel_rect(
    r: &Rect,
    display: &DisplayInfo,
    padding_px: u32,
    width: u32,
    height: u32,
) -> Option<Rect> {
    let s = display.scale;
    if !s.is_finite() || s <= 0.0 || r.is_empty() {
        return None;
    }
    let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    let local = Rect::new(
        clamp(r.x as i64 - display.origin.0 as i64),
        clamp(r.y as i64 - display.origin.1 as i64),
        r.w,
        r.h,
    );
    let pad = padding_px.min(i32::MAX as u32) as i32;
    local
        .scaled(s)
        .padded(pad)
        .clamp_to(&Rect::new(0, 0, width, height))
}

/// Black out `rects` (global points). Returns the pixel rects actually filled, or `None` if the
/// frame buffer is inconsistent, the display scale is unusable, or any rect has zero area
/// (caller must then block). Off-frame rects are ignored (nothing to hide).
pub(crate) fn blackout(frame: &mut Frame, rects: &[Rect], padding_px: u32) -> Option<Vec<Rect>> {
    let s = frame.display().scale;
    if !s.is_finite() || s <= 0.0 {
        return None;
    }
    let display = *frame.display();
    let (w, h, stride) = (frame.width(), frame.height(), frame.stride() as usize);
    if frame.pixels().len() < stride.checked_mul(h as usize)? {
        return None;
    }
    let mut applied = Vec::new();
    for r in rects {
        // A zero-area secure-field rect cannot be hidden or verified: fail closed.
        if r.is_empty() {
            return None;
        }
        // Rects wholly outside the frame yield `None` below and are skipped: there are no
        // pixels of that field in this frame to hide.
        let Some(p) = to_pixel_rect(r, &display, padding_px, w, h) else {
            continue;
        };
        let px = frame.pixels_mut();
        for y in p.y as usize..(p.y as usize + p.h as usize) {
            let start = y * stride + p.x as usize * 4;
            let end = start + p.w as usize * 4;
            // `as_chunks_mut` needs Rust 1.88; our MSRV is 1.85, so keep `chunks_exact_mut`.
            #[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
            for c in px.get_mut(start..end)?.chunks_exact_mut(4) {
                c.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        applied.push(p);
    }
    Some(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame::DisplayId;
    use std::time::Instant;

    fn frame(w: u32, h: u32, scale: f32, origin: (i32, i32)) -> Frame {
        let d = DisplayInfo {
            id: DisplayId(1),
            origin,
            size: ((w as f32 / scale) as u32, (h as f32 / scale) as u32),
            scale,
        };
        Frame::new(
            vec![200; (w * h * 4) as usize],
            w,
            h,
            w * 4,
            d,
            Instant::now(),
            vec![],
        )
        .unwrap()
    }

    fn is_black(f: &Frame, x: u32, y: u32) -> bool {
        let i = ((y * f.width() + x) * 4) as usize;
        f.pixels()[i..i + 4] == [0, 0, 0, 255]
    }

    #[test]
    fn fills_rect_and_leaves_rest() {
        let mut f = frame(40, 40, 1.0, (0, 0));
        let a = blackout(&mut f, &[Rect::new(10, 10, 5, 5)], 0).unwrap();
        assert_eq!(a, vec![Rect::new(10, 10, 5, 5)]);
        for y in 0..40 {
            for x in 0..40 {
                let inside = (10..15).contains(&x) && (10..15).contains(&y);
                assert_eq!(is_black(&f, x, y), inside, "{x},{y}");
                if !inside {
                    let i = ((y * 40 + x) * 4) as usize;
                    assert_eq!(f.pixels()[i..i + 4], [200; 4]);
                }
            }
        }
    }

    #[test]
    fn padding_applies() {
        let mut f = frame(40, 40, 1.0, (0, 0));
        let a = blackout(&mut f, &[Rect::new(10, 10, 5, 5)], 4).unwrap();
        assert_eq!(a, vec![Rect::new(6, 6, 13, 13)]);
        assert!(is_black(&f, 6, 6) && is_black(&f, 18, 18));
        assert!(!is_black(&f, 5, 6) && !is_black(&f, 19, 18));
    }

    #[test]
    fn clamps_to_frame() {
        let mut f = frame(20, 20, 1.0, (0, 0));
        let a = blackout(
            &mut f,
            &[Rect::new(-10, -10, 15, 15), Rect::new(15, 15, 50, 50)],
            0,
        )
        .unwrap();
        assert_eq!(a, vec![Rect::new(0, 0, 5, 5), Rect::new(15, 15, 5, 5)]);
        assert!(is_black(&f, 0, 0) && is_black(&f, 19, 19));
        assert!(!is_black(&f, 5, 5));
    }

    #[test]
    fn scale_2x_and_origin() {
        // Display at global origin (100, 50) in points, 2 px per point.
        let mut f = frame(100, 100, 2.0, (100, 50));
        let a = blackout(&mut f, &[Rect::new(110, 60, 5, 5)], 0).unwrap();
        assert_eq!(a, vec![Rect::new(20, 20, 10, 10)]);
        assert!(is_black(&f, 20, 20) && is_black(&f, 29, 29));
        assert!(!is_black(&f, 30, 30) && !is_black(&f, 19, 20));
    }

    #[test]
    fn empty_offscreen_and_oversized() {
        let mut f = frame(20, 20, 1.0, (0, 0));
        let a = blackout(
            &mut f,
            &[Rect::new(100, 100, 5, 5), Rect::new(-100, -100, 10, 10)],
            4,
        )
        .unwrap();
        assert!(a.is_empty());
        assert!(!is_black(&f, 5, 5));
        let a = blackout(&mut f, &[Rect::new(-5, -5, u32::MAX, u32::MAX)], 4).unwrap();
        assert_eq!(a, vec![Rect::new(0, 0, 20, 20)]);
        assert!(is_black(&f, 19, 19));
    }

    #[test]
    fn zero_area_rect_fails() {
        for r in [Rect::new(5, 5, 0, 5), Rect::new(5, 5, 5, 0)] {
            let mut f = frame(20, 20, 1.0, (0, 0));
            assert!(blackout(&mut f, &[r], 4).is_none());
        }
    }

    #[test]
    fn overflow_safe() {
        let mut f = frame(20, 20, 1.0, (i32::MIN, i32::MAX));
        assert!(blackout(
            &mut f,
            &[Rect::new(i32::MAX, i32::MIN, u32::MAX, u32::MAX)],
            u32::MAX
        )
        .is_some());
        let mut f = frame(20, 20, 1.0, (0, 0));
        assert!(blackout(
            &mut f,
            &[Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX)],
            4
        )
        .is_some());
    }

    #[test]
    fn bad_scale_fails() {
        for s in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let mut f = frame(10, 10, 1.0, (0, 0));
            let mut p = f.into_parts();
            p.display.scale = s;
            f = Frame::from_parts(p).unwrap();
            assert!(blackout(&mut f, &[Rect::new(0, 0, 5, 5)], 0).is_none());
        }
    }

    #[test]
    fn respects_stride_padding_bytes() {
        let d = DisplayInfo {
            id: DisplayId(1),
            origin: (0, 0),
            size: (4, 2),
            scale: 1.0,
        };
        let mut f = Frame::new(vec![7; 24 * 2], 4, 2, 24, d, Instant::now(), vec![]).unwrap();
        blackout(&mut f, &[Rect::new(0, 0, 4, 2)], 0).unwrap();
        assert_eq!(f.pixels()[16..24], [7; 8]); // row padding untouched
        assert_eq!(f.pixels()[0..4], [0, 0, 0, 255]);
    }
}
