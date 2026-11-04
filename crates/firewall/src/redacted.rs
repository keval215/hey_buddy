use std::fmt;
use std::sync::Arc;

use frame::{DisplayInfo, Frame, Rect};

/// A frame that has passed through the privacy firewall. Private fields; the only constructor
/// is `pub(crate)` and is called by `PrivacyFirewall::check` after a clear decision.
/// The pixel buffer is shared (`Arc`) and read-only, so `Clone` is cheap and infallible.
/// No `Deserialize`, `Default` or `From<Frame>`; `Debug` prints sizes only.
///
/// ```compile_fail,E0451
/// // Struct-literal construction from outside the crate: private fields (`todo!()` values
/// // keep type errors out of the way, so privacy is the only possible failure).
/// fn forge() -> firewall::RedactedFrame {
///     firewall::RedactedFrame { inner: todo!(), redactions: todo!() }
/// }
/// ```
///
/// ```compile_fail,E0624
/// // The constructor is `pub(crate)`; arguments are well-typed, so privacy is the only failure.
/// fn forge(f: frame::Frame) -> firewall::RedactedFrame {
///     firewall::RedactedFrame::from_blackout(f, Vec::new())
/// }
/// ```
///
/// ```compile_fail,E0308
/// // No conversion from a raw frame.
/// fn forge(f: frame::Frame) -> firewall::RedactedFrame {
///     firewall::RedactedFrame::from(f)
/// }
/// ```
///
/// ```compile_fail,E0599
/// // No Default.
/// let _ = firewall::RedactedFrame::default();
/// ```
#[derive(Clone)]
pub struct RedactedFrame {
    inner: Arc<Frame>,
    redactions: Vec<Rect>,
}

impl RedactedFrame {
    pub(crate) fn from_blackout(f: Frame, rects: Vec<Rect>) -> Self {
        Self {
            inner: Arc::new(f),
            redactions: rects,
        }
    }

    pub fn width(&self) -> u32 {
        self.inner.width()
    }
    pub fn height(&self) -> u32 {
        self.inner.height()
    }
    /// Bytes per row.
    pub fn stride(&self) -> u32 {
        self.inner.stride()
    }
    /// RGBA8 pixels with all redactions already applied.
    pub fn pixels(&self) -> &[u8] {
        self.inner.pixels()
    }
    /// Pixel rects that were blacked out (including padding, clamped to the frame).
    pub fn redactions(&self) -> &[Rect] {
        &self.redactions
    }
    pub fn display(&self) -> &DisplayInfo {
        self.inner.display()
    }
}

impl fmt::Debug for RedactedFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RedactedFrame({}x{}, stride {}, {} redactions)",
            self.width(),
            self.height(),
            self.stride(),
            self.redactions.len()
        )
    }
}
