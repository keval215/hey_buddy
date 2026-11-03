use std::fmt;
use std::time::Instant;

use crate::{DisplayInfo, WindowInfo};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame width and height must be non-zero")]
    EmptyDimensions,
    #[error("stride is smaller than one row of RGBA8 pixels")]
    StrideTooSmall,
    #[error("pixel buffer length does not match stride and height")]
    BadBufferLength,
    #[error("frame dimensions overflow")]
    Overflow,
}

/// A captured screen image: RGBA8, row-major. Private fields; not `Clone`; no serde.
/// `Debug` prints sizes only, never pixels.
pub struct Frame {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    stride: u32,
    display: DisplayInfo,
    captured_at: Instant,
    excluded_windows: Vec<WindowInfo>,
}

/// The pieces of a consumed [`Frame`].
///
/// Only the privacy firewall should call [`Frame::into_parts`] / [`Frame::from_parts`]; Rust
/// cannot forbid other crates from doing so, so this is enforced by review and by `llm` having
/// no API that accepts a `Frame`.
pub struct FrameParts {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub display: DisplayInfo,
    pub captured_at: Instant,
    pub excluded_windows: Vec<WindowInfo>,
}

impl fmt::Debug for FrameParts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameParts")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// `stride` is bytes per row (>= width * 4). `pixels.len()` must equal `stride * height`.
    pub fn new(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        display: DisplayInfo,
        captured_at: Instant,
        excluded_windows: Vec<WindowInfo>,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::EmptyDimensions);
        }
        let row = width.checked_mul(4).ok_or(FrameError::Overflow)?;
        if stride < row {
            return Err(FrameError::StrideTooSmall);
        }
        let expected = (stride as u64) * (height as u64);
        if pixels.len() as u64 != expected {
            return Err(FrameError::BadBufferLength);
        }
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            display,
            captured_at,
            excluded_windows,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Bytes per row.
    pub fn stride(&self) -> u32 {
        self.stride
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn display(&self) -> &DisplayInfo {
        &self.display
    }
    pub fn captured_at(&self) -> Instant {
        self.captured_at
    }
    /// Deny-listed windows that were removed at capture time.
    pub fn excluded_windows(&self) -> &[WindowInfo] {
        &self.excluded_windows
    }

    /// Mutable pixel access for the privacy firewall's blackout. Not for other callers.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Consume the frame. Intended for the privacy firewall only (see [`FrameParts`]).
    pub fn into_parts(self) -> FrameParts {
        FrameParts {
            pixels: self.pixels,
            width: self.width,
            height: self.height,
            stride: self.stride,
            display: self.display,
            captured_at: self.captured_at,
            excluded_windows: self.excluded_windows,
        }
    }

    /// Rebuild a frame from parts, re-validating. Intended for the privacy firewall only.
    pub fn from_parts(p: FrameParts) -> Result<Self, FrameError> {
        Self::new(
            p.pixels,
            p.width,
            p.height,
            p.stride,
            p.display,
            p.captured_at,
            p.excluded_windows,
        )
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame({}x{}, stride {})",
            self.width, self.height, self.stride
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DisplayId;

    fn disp() -> DisplayInfo {
        DisplayInfo {
            id: DisplayId(1),
            origin: (0, 0),
            size: (2, 2),
            scale: 1.0,
        }
    }

    fn mk(buf: Vec<u8>, w: u32, h: u32, s: u32) -> Result<Frame, FrameError> {
        Frame::new(buf, w, h, s, disp(), Instant::now(), vec![])
    }

    #[test]
    fn validates() {
        assert!(mk(vec![0; 16], 2, 2, 8).is_ok());
        assert!(mk(vec![0; 24], 2, 2, 12).is_ok());
        assert_eq!(
            mk(vec![], 0, 2, 0).unwrap_err(),
            FrameError::EmptyDimensions
        );
        assert_eq!(
            mk(vec![0; 12], 2, 2, 6).unwrap_err(),
            FrameError::StrideTooSmall
        );
        assert_eq!(
            mk(vec![0; 15], 2, 2, 8).unwrap_err(),
            FrameError::BadBufferLength
        );
        assert_eq!(
            mk(vec![], u32::MAX, 1, u32::MAX).unwrap_err(),
            FrameError::Overflow
        );
    }

    #[test]
    fn debug_has_no_pixels() {
        let f = mk(vec![0xAB; 16], 2, 2, 8).unwrap();
        let s = format!("{f:?}");
        assert_eq!(s, "Frame(2x2, stride 8)");
        assert!(!format!("{:?}", f.into_parts()).contains("171"));
    }

    #[test]
    fn parts_round_trip_and_mutate() {
        let mut f = mk(vec![1; 16], 2, 2, 8).unwrap();
        f.pixels_mut()[0] = 9;
        let f = Frame::from_parts(f.into_parts()).unwrap();
        assert_eq!(f.pixels()[0], 9);
    }
}
