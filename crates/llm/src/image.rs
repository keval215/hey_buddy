//! Image path (ADR 0002 section 4). Input is only a [`firewall::RedactedFrame`]; the pixels
//! are area-averaged down in our own code (no `image` crate), converted to RGB8 and PNG-encoded
//! exactly once per attempt.

use std::sync::Arc;

use firewall::RedactedFrame;
use frame::DisplayId;

use crate::LlmError;

/// Longest edge of the image we send.
pub const MAX_LONG_EDGE: u32 = 1568;
/// Maximum pixel count of the image we send (1.15 megapixels).
pub const MAX_PIXELS: u64 = 1_150_000;
/// Maximum encoded PNG size.
pub const MAX_PNG_BYTES: usize = 3_750_000;
/// Number of shrink-and-retry attempts after the first encode.
const MAX_RETRIES: u32 = 3;

/// Where the sent image sits in global screen space, so POINT coordinates can be mapped back:
/// `g = origin + px * (frame_px / sent_px) / scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageGeometry {
    pub display_id: DisplayId,
    /// Global origin of the display (points on macOS, pixels on Windows).
    pub origin: (i32, i32),
    /// Pixels per global unit.
    pub scale: f32,
    /// Size of the (redacted) captured frame in pixels.
    pub frame_px: (u32, u32),
    /// Size of the image actually sent, in pixels.
    pub sent_px: (u32, u32),
}

/// A downscaled, PNG-encoded redacted frame. Only constructible from a `RedactedFrame`.
#[derive(Clone)]
pub(crate) struct EncodedImage {
    pub png: Arc<[u8]>,
    pub geometry: ImageGeometry,
}

/// Target size for a source of `w x h` after multiplying the fit-scale by `factor`.
fn target_dims(w: u32, h: u32, factor: f64) -> (u32, u32) {
    let (wf, hf) = (f64::from(w), f64::from(h));
    let by_edge = f64::from(MAX_LONG_EDGE) / wf.max(hf);
    let by_area = (MAX_PIXELS as f64 / (wf * hf)).sqrt();
    let s = 1.0f64.min(by_edge).min(by_area) * factor;
    let mut tw = ((wf * s).floor() as u32).clamp(1, w);
    let mut th = ((hf * s).floor() as u32).clamp(1, h);
    // Float rounding guard: the bounds above are hard limits.
    while tw > MAX_LONG_EDGE || th > MAX_LONG_EDGE || u64::from(tw) * u64::from(th) > MAX_PIXELS {
        tw = (tw - 1).max(1);
        th = (th - 1).max(1);
        if tw == 1 && th == 1 {
            break;
        }
    }
    (tw, th)
}

/// Area-average downscale of RGBA8 (row stride `stride` bytes) to tightly packed RGB8.
/// Each destination pixel is the mean of the source box it covers, so a solid black region
/// stays black (only its boundary pixels blend with neighbours).
fn downscale_rgb(src: &[u8], w: u32, h: u32, stride: u32, tw: u32, th: u32) -> Vec<u8> {
    let (w, h, stride, tw, th) = (
        w as usize,
        h as usize,
        stride as usize,
        tw as usize,
        th as usize,
    );
    let mut out = Vec::with_capacity(tw * th * 3);
    for dy in 0..th {
        let y0 = dy * h / th;
        let y1 = (((dy + 1) * h).div_ceil(th)).clamp(y0 + 1, h);
        for dx in 0..tw {
            let x0 = dx * w / tw;
            let x1 = (((dx + 1) * w).div_ceil(tw)).clamp(x0 + 1, w);
            let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
            for y in y0..y1 {
                let row = &src[y * stride + x0 * 4..y * stride + x1 * 4];
                for px in row.chunks_exact(4) {
                    r += u64::from(px[0]);
                    g += u64::from(px[1]);
                    b += u64::from(px[2]);
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.push(((r + n / 2) / n) as u8);
            out.push(((g + n / 2) / n) as u8);
            out.push(((b + n / 2) / n) as u8);
        }
    }
    out
}

fn encode_png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, LlmError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|_| LlmError::Encode("png_encode"))?;
        writer
            .write_image_data(rgb)
            .map_err(|_| LlmError::Encode("png_encode"))?;
    }
    Ok(out)
}

/// Downscale + encode `frame`, retrying at x0.75 up to three times if the PNG exceeds
/// `max_bytes`, else `Encode("image_too_large")`.
pub(crate) fn encode_frame_with_limit(
    frame: &RedactedFrame,
    max_bytes: usize,
) -> Result<EncodedImage, LlmError> {
    let (w, h, stride) = (frame.width(), frame.height(), frame.stride());
    let pixels = frame.pixels();
    if w == 0
        || h == 0
        || u64::from(stride) < u64::from(w) * 4
        || (pixels.len() as u64) < u64::from(stride) * u64::from(h)
    {
        return Err(LlmError::Encode("bad_frame"));
    }
    let mut factor = 1.0f64;
    for _ in 0..=MAX_RETRIES {
        let (tw, th) = target_dims(w, h, factor);
        let rgb = downscale_rgb(pixels, w, h, stride, tw, th);
        let png = encode_png(&rgb, tw, th)?;
        if png.len() <= max_bytes {
            let d = frame.display();
            return Ok(EncodedImage {
                png: Arc::from(png),
                geometry: ImageGeometry {
                    display_id: d.id,
                    origin: d.origin,
                    scale: d.scale,
                    frame_px: (w, h),
                    sent_px: (tw, th),
                },
            });
        }
        factor *= 0.75;
    }
    Err(LlmError::Encode("image_too_large"))
}

pub(crate) fn encode_frame(frame: &RedactedFrame) -> Result<EncodedImage, LlmError> {
    encode_frame_with_limit(frame, MAX_PNG_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{clear_frame, clear_frame_with};

    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let dec = png::Decoder::new(std::io::Cursor::new(png));
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn dims_respect_edge_and_area_limits() {
        for (w, h) in [
            (3840, 2160),
            (1920, 1080),
            (5000, 400),
            (400, 5000),
            (1568, 1568),
        ] {
            let (tw, th) = target_dims(w, h, 1.0);
            assert!(tw <= MAX_LONG_EDGE && th <= MAX_LONG_EDGE, "{w}x{h}");
            assert!(u64::from(tw) * u64::from(th) <= MAX_PIXELS, "{w}x{h}");
            assert!(tw >= 1 && th >= 1);
        }
        // Small images are never upscaled.
        assert_eq!(target_dims(800, 600, 1.0), (800, 600));
        // Retry factor shrinks further.
        let (a, _) = target_dims(1920, 1080, 1.0);
        let (b, _) = target_dims(1920, 1080, 0.75);
        assert!(b < a);
    }

    #[test]
    fn large_frame_is_downscaled_and_geometry_recorded() {
        let rf = clear_frame_with(3000, 2000, vec![]);
        let img = encode_frame(&rf).unwrap();
        let (w, h, rgb) = decode(&img.png);
        assert!(w <= MAX_LONG_EDGE && h <= MAX_LONG_EDGE);
        assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
        assert_eq!(rgb.len() as u32, w * h * 3);
        let g = img.geometry;
        assert_eq!(g.frame_px, (3000, 2000));
        assert_eq!(g.sent_px, (w, h));
        assert_eq!(g.display_id, rf.display().id);
        assert_eq!(g.origin, rf.display().origin);
        assert_eq!(g.scale, rf.display().scale);
    }

    #[test]
    fn black_redaction_rect_stays_black_after_downscale() {
        use frame::Rect;
        let rect = Rect::new(1000, 800, 600, 200);
        let rf = clear_frame_with(3000, 2000, vec![rect]);
        assert_eq!(rf.redactions().len(), 1);
        let img = encode_frame(&rf).unwrap();
        let (w, h, rgb) = decode(&img.png);
        let sx = f64::from(w) / 3000.0;
        let sy = f64::from(h) / 2000.0;
        let padded = rf.redactions()[0];
        // Interior of the padded rect (1 destination pixel inset) must be pure black.
        let x0 = (f64::from(padded.x) * sx).ceil() as u32 + 1;
        let x1 = (f64::from(padded.x + padded.w as i32) * sx).floor() as u32 - 1;
        let y0 = (f64::from(padded.y) * sy).ceil() as u32 + 1;
        let y1 = (f64::from(padded.y + padded.h as i32) * sy).floor() as u32 - 1;
        assert!(x1 > x0 && y1 > y0);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 3) as usize;
                assert_eq!(&rgb[i..i + 3], &[0, 0, 0], "pixel {x},{y} leaked");
            }
        }
        // Outside the rect the (non-black) background survives.
        assert_ne!(&rgb[0..3], &[0, 0, 0]);
    }

    #[test]
    fn oversize_png_retries_then_fails_closed() {
        let rf = clear_frame(64, 64);
        // Limit of 1 byte can never be met: 4 attempts, then image_too_large.
        assert_eq!(
            encode_frame_with_limit(&rf, 1).err(),
            Some(LlmError::Encode("image_too_large"))
        );
    }

    #[test]
    fn retry_shrinks_image_to_fit() {
        let rf = clear_frame(1200, 900);
        let full = encode_frame(&rf).unwrap();
        // A limit just below the first attempt forces at least one x0.75 retry.
        let img = encode_frame_with_limit(&rf, full.png.len() - 1).unwrap();
        assert!(img.geometry.sent_px.0 < full.geometry.sent_px.0);
        assert!(img.png.len() < full.png.len());
    }
}
