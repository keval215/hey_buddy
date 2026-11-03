use std::fmt;
use std::sync::Arc;

use frame::Rect;

use crate::text::TextCoverage;

/// Screen text that has passed through the privacy firewall (`check_text`). Private fields;
/// the only constructor is `pub(crate)` and is called by `redact_tree`. Cheap `Clone`
/// (shared `Arc`s). No `Deserialize`, `Default` or `From<String>`; `Debug` prints counts only.
///
/// ```compile_fail,E0451
/// // Struct-literal construction from outside the crate: private fields (`todo!()` values
/// // keep type errors out of the way, so privacy is the only possible failure).
/// fn forge() -> firewall::RedactedScreenText {
///     firewall::RedactedScreenText {
///         text: todo!(),
///         nodes: todo!(),
///         window: todo!(),
///         coverage: todo!(),
///         secure_hidden: todo!(),
///     }
/// }
/// ```
///
/// ```compile_fail,E0624
/// // The constructor is `pub(crate)`; arguments are well-typed, so privacy is the only failure.
/// fn forge(c: firewall::TextCoverage) -> firewall::RedactedScreenText {
///     firewall::RedactedScreenText::from_tree(
///         std::sync::Arc::from("x"),
///         std::sync::Arc::from(Vec::<frame::Rect>::new()),
///         frame::Rect::new(0, 0, 1, 1),
///         c,
///         0,
///     )
/// }
/// ```
///
/// ```compile_fail,E0308
/// // No conversion from a raw string.
/// fn forge(s: String) -> firewall::RedactedScreenText {
///     firewall::RedactedScreenText::from(s)
/// }
/// ```
///
/// ```compile_fail,E0599
/// // No Default.
/// let _ = firewall::RedactedScreenText::default();
/// ```
#[derive(Clone)]
pub struct RedactedScreenText {
    text: Arc<str>,
    /// Index = `#id - 1`; rects clipped to the window.
    nodes: Arc<[Rect]>,
    window: Rect,
    coverage: TextCoverage,
    secure_hidden: u32,
}

impl RedactedScreenText {
    pub(crate) fn from_tree(
        text: Arc<str>,
        nodes: Arc<[Rect]>,
        window: Rect,
        coverage: TextCoverage,
        secure_hidden: u32,
    ) -> Self {
        Self {
            text,
            nodes,
            window,
            coverage,
            secure_hidden,
        }
    }

    /// The "screen-text v1" text.
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }

    /// Rect (global space, clipped to the window) of the node numbered `id` (1-based).
    pub fn node_rect(&self, id: u32) -> Option<Rect> {
        let i = usize::try_from(id).ok()?.checked_sub(1)?;
        self.nodes.get(i).copied()
    }

    pub fn window_rect(&self) -> Rect {
        self.window
    }

    pub fn coverage(&self) -> TextCoverage {
        self.coverage
    }

    pub fn secure_fields_hidden(&self) -> u32 {
        self.secure_hidden
    }
}

impl fmt::Debug for RedactedScreenText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RedactedScreenText({} nodes, {} chars, {} secure hidden)",
            self.nodes.len(),
            self.text.chars().count(),
            self.secure_hidden
        )
    }
}
