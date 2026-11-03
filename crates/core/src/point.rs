//! POINT tag parsing and mapping (ADR 0002 section 10). Pure: no I/O.
//!
//! The LLM may embed `[POINT:x,y:label]` (image form, `x,y` in SENT-image pixels, a legacy
//! `:screenN` suffix is ignored) or `[POINT:#id:label]` (node form, `id` of a screen-text node)
//! in its reply. [`PointParser`] consumes text deltas incrementally, returns the text with all
//! POINT tags removed (what TTS may speak) and the raw tags. [`map_tag`] turns a tag into a
//! [`PointEvent`] in global points, or drops it silently.
//!
//! Labels are model output: display-only, sanitised here (control chars stripped, capped), and
//! never logged by this module.

use frame::{DisplayId, DisplayInfo, Rect};

use crate::api::PointEvent;

const PREFIX: &str = "[POINT:";
/// A tag longer than this (in chars, brackets included) cannot be a tag: flushed as text.
pub const MAX_TAG_CHARS: usize = 256;
/// Labels are cut to this many chars.
pub const MAX_LABEL_CHARS: usize = 80;
/// Image coordinates further than this many times the sent-image size are nonsense: dropped.
const MAX_OVERSHOOT: f64 = 2.0;

/// A parsed, not yet mapped tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointTag {
    /// `[POINT:x,y:label]`, `x`,`y` in sent-image pixels.
    Image { x: u32, y: u32, label: String },
    /// `[POINT:#id:label]`.
    Node { id: u32, label: String },
}

/// Output of one `push`/`finish`: tag-free text plus the tags found, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseOutput {
    pub text: String,
    pub tags: Vec<PointTag>,
}

/// Incremental tag extractor. Feed deltas with [`push`](Self::push), then call
/// [`finish`](Self::finish) once at the end of the reply.
#[derive(Debug, Default)]
pub struct PointParser {
    /// Possible start of a tag held back until it resolves. Always starts with `[`.
    pending: String,
}

impl PointParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) -> ParseOutput {
        self.pending.push_str(delta);
        let mut out = ParseOutput::default();
        let mut rest = std::mem::take(&mut self.pending);
        loop {
            let Some(i) = rest.find('[') else {
                out.text.push_str(&rest);
                return out;
            };
            out.text.push_str(&rest[..i]);
            let s = &rest[i..];
            match classify(s) {
                Cls::NotTag => {
                    out.text.push('[');
                    rest = s[1..].to_string();
                }
                Cls::NeedMore => {
                    self.pending = s.to_string();
                    return out;
                }
                Cls::Tag(end) => {
                    if let Some(t) = parse_body(&s[PREFIX.len()..end]) {
                        out.tags.push(t);
                    }
                    rest = s[end + 1..].to_string();
                }
            }
        }
    }

    /// End of reply. An unfinished `[POINT:` tag is dropped (never spoken); a held-back partial
    /// prefix such as `[PO` was ordinary text and is returned.
    pub fn finish(&mut self) -> ParseOutput {
        let p = std::mem::take(&mut self.pending);
        let mut out = ParseOutput::default();
        if !p.starts_with(PREFIX) {
            out.text = p;
        }
        out
    }
}

enum Cls {
    NotTag,
    NeedMore,
    /// Index of the closing `]` within the slice.
    Tag(usize),
}

/// `s` starts with `[`.
fn classify(s: &str) -> Cls {
    if s.len() < PREFIX.len() {
        return if PREFIX.starts_with(s) {
            Cls::NeedMore
        } else {
            Cls::NotTag
        };
    }
    if !s.starts_with(PREFIX) {
        return Cls::NotTag;
    }
    for (n, (i, c)) in s.char_indices().enumerate() {
        if n >= MAX_TAG_CHARS {
            return Cls::NotTag;
        }
        if n < PREFIX.len() {
            continue;
        }
        match c {
            ']' => return Cls::Tag(i),
            '\n' | '[' => return Cls::NotTag,
            _ => {}
        }
    }
    Cls::NeedMore
}

fn parse_body(body: &str) -> Option<PointTag> {
    if let Some(rest) = body.strip_prefix('#') {
        let (id, label) = match rest.split_once(':') {
            Some((id, l)) => (id, l),
            None => (rest, ""),
        };
        return Some(PointTag::Node {
            id: parse_num(id)?,
            label: sanitize_label(label),
        });
    }
    let (coords, label) = match body.split_once(':') {
        Some((c, l)) => (c, l),
        None => (body, ""),
    };
    let (x, y) = coords.split_once(',')?;
    Some(PointTag::Image {
        x: parse_num(x)?,
        y: parse_num(y)?,
        label: sanitize_label(strip_screen_suffix(label)),
    })
}

/// Digits only, at most 7 of them.
fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() || s.len() > 7 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Remove a legacy trailing `:screenN` (a label that is only `screenN` becomes empty).
fn strip_screen_suffix(label: &str) -> &str {
    fn is_screen(t: &str) -> bool {
        t.trim()
            .strip_prefix("screen")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }
    match label.rsplit_once(':') {
        Some((head, tail)) if is_screen(tail) => head,
        None if is_screen(label) => "",
        _ => label,
    }
}

/// Strip control chars, trim, cap at [`MAX_LABEL_CHARS`] chars.
pub fn sanitize_label(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_LABEL_CHARS).collect()
}

// ---- mapping ----

/// Geometry of the image that was sent to the model (a local mirror of the `llm` crate's
/// `ImageGeometry`; T12 converts).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageGeometry {
    /// Display origin in global points.
    pub origin: (i32, i32),
    /// Pixels per point of the display.
    pub scale: f32,
    /// Captured frame size in pixels.
    pub frame_px: (u32, u32),
    /// Size of the image actually sent (may be downscaled), in pixels.
    pub sent_px: (u32, u32),
    pub display_id: DisplayId,
}

/// Screen-text context for node-form tags.
pub struct NodeContext<'a> {
    /// Rect of the window the text came from, global points.
    pub window_rect: Rect,
    /// Displays, to resolve the display of the pointed-at point.
    pub displays: &'a [DisplayInfo],
    /// `RedactedScreenText::node_rect(id)`, global points.
    pub node_rect: &'a dyn Fn(u32) -> Option<Rect>,
}

/// Which tag form this request allows.
pub enum PointContext<'a> {
    /// The request carried an image: only the image form is valid.
    Image(ImageGeometry),
    /// The request carried screen text: only the node form is valid.
    Node(NodeContext<'a>),
    /// No screen context: every tag is dropped.
    None,
}

/// The mode without its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointMode {
    Image,
    Node,
    None,
}

impl PointContext<'_> {
    pub fn mode(&self) -> PointMode {
        match self {
            PointContext::Image(_) => PointMode::Image,
            PointContext::Node(_) => PointMode::Node,
            PointContext::None => PointMode::None,
        }
    }
}

fn clamp_i64(v: i64, lo: i64, hi: i64) -> i32 {
    v.max(lo).min(hi.max(lo)) as i32
}

/// Image form -> global points: `g = origin + px*(frame_px/sent_px)/scale`, clamped to the
/// display bounds. `None` for bad geometry or a coordinate more than 2x outside the image.
pub fn map_image_point(g: &ImageGeometry, x: u32, y: u32) -> Option<(i32, i32)> {
    let (sw, sh) = (g.sent_px.0 as f64, g.sent_px.1 as f64);
    let (fw, fh) = (g.frame_px.0 as f64, g.frame_px.1 as f64);
    let scale = g.scale as f64;
    if sw < 1.0 || sh < 1.0 || fw < 1.0 || fh < 1.0 || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    if x as f64 > sw * MAX_OVERSHOOT || y as f64 > sh * MAX_OVERSHOOT {
        return None;
    }
    let gx = g.origin.0 as f64 + x as f64 * (fw / sw) / scale;
    let gy = g.origin.1 as f64 + y as f64 * (fh / sh) / scale;
    // Display size in points; last addressable point is size-1.
    let max_x = g.origin.0 as i64 + (fw / scale).floor() as i64 - 1;
    let max_y = g.origin.1 as i64 + (fh / scale).floor() as i64 - 1;
    Some((
        clamp_i64(gx.round() as i64, g.origin.0 as i64, max_x),
        clamp_i64(gy.round() as i64, g.origin.1 as i64, max_y),
    ))
}

fn contains(r: &Rect, x: i32, y: i32) -> bool {
    !r.is_empty()
        && (x as i64) >= r.x as i64
        && (y as i64) >= r.y as i64
        && (x as i64) < r.x as i64 + r.w as i64
        && (y as i64) < r.y as i64 + r.h as i64
}

/// Node form -> centre of the node rect, clamped to the window rect; the display is the one
/// containing that point (none -> dropped).
pub fn map_node_point(ctx: &NodeContext<'_>, id: u32) -> Option<(i32, i32, DisplayId)> {
    let r = (ctx.node_rect)(id)?;
    let w = &ctx.window_rect;
    if r.is_empty() || w.is_empty() {
        return None;
    }
    let cx = r.x as i64 + (r.w / 2) as i64;
    let cy = r.y as i64 + (r.h / 2) as i64;
    let x = clamp_i64(cx, w.x as i64, w.x as i64 + w.w as i64 - 1);
    let y = clamp_i64(cy, w.y as i64, w.y as i64 + w.h as i64 - 1);
    let d = ctx.displays.iter().find(|d| contains(&d.bounds(), x, y))?;
    Some((x, y, d.id))
}

/// Resolve one tag against the request context. `None` = drop silently.
pub fn map_tag(tag: &PointTag, ctx: &PointContext<'_>) -> Option<PointEvent> {
    match (tag, ctx) {
        (PointTag::Image { x, y, label }, PointContext::Image(g)) => {
            let (gx, gy) = map_image_point(g, *x, *y)?;
            Some(PointEvent {
                x: gx,
                y: gy,
                label: label.clone(),
                display_id: g.display_id.0,
            })
        }
        (PointTag::Node { id, label }, PointContext::Node(n)) => {
            let (x, y, d) = map_node_point(n, *id)?;
            Some(PointEvent {
                x,
                y,
                label: label.clone(),
                display_id: d.0,
            })
        }
        _ => None,
    }
}

/// Parser plus mapper for one reply.
pub struct PointStream<'a> {
    parser: PointParser,
    ctx: PointContext<'a>,
}

impl<'a> PointStream<'a> {
    pub fn new(ctx: PointContext<'a>) -> Self {
        Self {
            parser: PointParser::new(),
            ctx,
        }
    }

    fn map(&self, o: ParseOutput) -> (String, Vec<PointEvent>) {
        let ev = o
            .tags
            .iter()
            .filter_map(|t| map_tag(t, &self.ctx))
            .collect();
        (o.text, ev)
    }

    /// Text to speak and the point events found in this delta.
    pub fn push(&mut self, delta: &str) -> (String, Vec<PointEvent>) {
        let o = self.parser.push(delta);
        self.map(o)
    }

    pub fn finish(&mut self) -> (String, Vec<PointEvent>) {
        let o = self.parser.finish();
        self.map(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(s: &str) -> ParseOutput {
        let mut p = PointParser::new();
        let mut o = p.push(s);
        let f = p.finish();
        o.text.push_str(&f.text);
        o.tags.extend(f.tags);
        o
    }

    fn img(x: u32, y: u32, l: &str) -> PointTag {
        PointTag::Image {
            x,
            y,
            label: l.into(),
        }
    }

    #[test]
    fn valid_image_and_node_forms() {
        let o = parse_all("Click [POINT:120,45:Save button] now");
        assert_eq!(o.text, "Click  now");
        assert_eq!(o.tags, vec![img(120, 45, "Save button")]);
        let o = parse_all("[POINT:#17:Search box]Type here");
        assert_eq!(o.text, "Type here");
        assert_eq!(
            o.tags,
            vec![PointTag::Node {
                id: 17,
                label: "Search box".into()
            }]
        );
    }

    #[test]
    fn legacy_screen_suffix_ignored() {
        let o = parse_all("[POINT:1,2:Menu:screen2]");
        assert_eq!(o.tags, vec![img(1, 2, "Menu")]);
        // A colon in the label that is not a screen suffix is kept.
        let o = parse_all("[POINT:1,2:Time: 10:30]");
        assert_eq!(o.tags, vec![img(1, 2, "Time: 10:30")]);
        let o = parse_all("[POINT:1,2:screen3]");
        assert_eq!(o.tags, vec![img(1, 2, "")]);
    }

    #[test]
    fn label_optional_and_multiple_tags() {
        let o = parse_all("a [POINT:3,4] b [POINT:#2] c [POINT:5,6:x] d");
        assert_eq!(o.text, "a  b  c  d");
        assert_eq!(o.tags.len(), 3);
    }

    #[test]
    fn malformed_removed_without_event() {
        for bad in [
            "[POINT:]",
            "[POINT:abc]",
            "[POINT:1:label]",
            "[POINT:1,:x]",
            "[POINT:,2:x]",
            "[POINT:-1,2:x]",
            "[POINT:1.5,2:x]",
            "[POINT:#:x]",
            "[POINT:#a:x]",
            "[POINT:99999999,1:x]",
            "[POINT:1,2,3:x]",
        ] {
            let o = parse_all(&format!("before{bad}after"));
            assert_eq!(o.text, "beforeafter", "{bad}");
            assert!(o.tags.is_empty(), "{bad}");
        }
    }

    #[test]
    fn non_tags_stay_text() {
        for s in [
            "plain",
            "[not a tag]",
            "array[0] and [1]",
            "[point:1,2:x]",
            "[POINT 1,2]",
            "[[POINT:1,2:x]",
        ] {
            let o = parse_all(s);
            if s == "[[POINT:1,2:x]" {
                assert_eq!(o.text, "[");
                assert_eq!(o.tags.len(), 1);
            } else {
                assert_eq!(o.text, s);
                assert!(o.tags.is_empty());
            }
        }
    }

    #[test]
    fn newline_inside_tag_flushes_as_text() {
        let o = parse_all("[POINT:1,2\nmore] text");
        assert_eq!(o.text, "[POINT:1,2\nmore] text");
        assert!(o.tags.is_empty());
    }

    #[test]
    fn overlong_unclosed_tag_flushes_as_text_bounded() {
        let mut p = PointParser::new();
        let junk = "x".repeat(400);
        let o = p.push(&format!("[POINT:{junk}"));
        assert!(o.tags.is_empty());
        assert!(o.text.starts_with("[POINT:"));
        assert_eq!(o.text.len(), 7 + 400);
        assert!(p.pending.is_empty());
        // Held-back buffer never exceeds the cap.
        let mut p = PointParser::new();
        for _ in 0..1000 {
            p.push("[POINT:1");
            assert!(p.pending.chars().count() <= MAX_TAG_CHARS);
        }
    }

    #[test]
    fn unfinished_tag_at_end_is_dropped_but_partial_prefix_is_text() {
        let mut p = PointParser::new();
        let o = p.push("hi [POINT:12,3");
        assert_eq!(o.text, "hi ");
        assert_eq!(p.finish(), ParseOutput::default());
        let mut p = PointParser::new();
        let o = p.push("hi [PO");
        assert_eq!(o.text, "hi ");
        assert_eq!(p.finish().text, "[PO");
    }

    #[test]
    fn split_at_every_byte_offset() {
        let text = "Go [POINT:10,20:Résumé ✓] then [POINT:#4:ok:screen1] and [POINT:x] end [PO";
        let whole = parse_all(text);
        assert_eq!(whole.text, "Go  then  and  end [PO");
        assert_eq!(whole.tags.len(), 2);
        for i in 0..=text.len() {
            if !text.is_char_boundary(i) {
                continue;
            }
            let mut p = PointParser::new();
            let a = p.push(&text[..i]);
            let b = p.push(&text[i..]);
            let f = p.finish();
            let got_text = format!("{}{}{}", a.text, b.text, f.text);
            let mut tags = a.tags;
            tags.extend(b.tags);
            tags.extend(f.tags);
            assert_eq!(got_text, whole.text, "split {i}");
            assert_eq!(tags, whole.tags, "split {i}");
        }
        // One char at a time.
        let mut p = PointParser::new();
        let mut t = String::new();
        let mut tags = vec![];
        for c in text.chars() {
            let o = p.push(&c.to_string());
            t.push_str(&o.text);
            tags.extend(o.tags);
        }
        let f = p.finish();
        t.push_str(&f.text);
        assert_eq!((t, tags), (whole.text, whole.tags));
    }

    #[test]
    fn no_tag_leakage_into_text() {
        let text = "a [POINT:1,2:x] b [POINT:#3:y] c [POINT:bad] d [POINT:5,6:z:screen2] e";
        let o = parse_all(text);
        assert!(!o.text.contains("POINT"));
        assert!(!o.text.contains('['));
        assert!(!o.text.contains(']'));
    }

    #[test]
    fn label_sanitising() {
        assert_eq!(sanitize_label("  a\u{0}b\tc\nd\u{7f}e  "), "abcde");
        let long = "y".repeat(200);
        assert_eq!(sanitize_label(&long).chars().count(), MAX_LABEL_CHARS);
        let multi = "é".repeat(200);
        assert_eq!(sanitize_label(&multi).chars().count(), MAX_LABEL_CHARS);
        let o = parse_all(&format!("[POINT:1,2:{long}]"));
        match &o.tags[0] {
            PointTag::Image { label, .. } => assert_eq!(label.chars().count(), 80),
            _ => panic!(),
        }
    }

    // ---- mapping ----

    fn geom(origin: (i32, i32), scale: f32, frame: (u32, u32), sent: (u32, u32)) -> ImageGeometry {
        ImageGeometry {
            origin,
            scale,
            frame_px: frame,
            sent_px: sent,
            display_id: DisplayId(7),
        }
    }

    #[test]
    fn image_mapping_basic_and_retina() {
        // 1x display at origin.
        let g = geom((0, 0), 1.0, (1920, 1080), (1920, 1080));
        assert_eq!(map_image_point(&g, 100, 200), Some((100, 200)));
        // Retina: 2x, frame 2880x1800 px = 1440x900 pt.
        let g = geom((0, 0), 2.0, (2880, 1800), (2880, 1800));
        assert_eq!(map_image_point(&g, 100, 200), Some((50, 100)));
    }

    #[test]
    fn image_mapping_downscaled_and_offset_origins() {
        // Retina 2x frame 2880x1800, sent downscaled to 1440x900: ratio 2, /scale 2 => 1:1.
        let g = geom((1440, 0), 2.0, (2880, 1800), (1440, 900));
        assert_eq!(map_image_point(&g, 10, 20), Some((1450, 20)));
        // Negative origin (monitor left of primary).
        let g = geom((-1920, -200), 1.0, (1920, 1080), (960, 540));
        assert_eq!(map_image_point(&g, 480, 270), Some((-960, 340)));
        // Fractional scale.
        let g = geom((0, 0), 1.5, (3000, 1500), (1000, 500));
        assert_eq!(map_image_point(&g, 500, 250), Some((1000, 500)));
    }

    #[test]
    fn image_mapping_clamps_to_display() {
        let g = geom((-1920, 0), 1.0, (1920, 1080), (1920, 1080));
        assert_eq!(map_image_point(&g, 1920, 1080), Some((-1, 1079)));
        assert_eq!(map_image_point(&g, 2500, 1500), Some((-1, 1079)));
        let g = geom((100, 50), 2.0, (2000, 1000), (1000, 500));
        // display is 1000x500 pt at (100,50): last point (1099, 549).
        assert_eq!(map_image_point(&g, 1000, 500), Some((1099, 549)));
    }

    #[test]
    fn image_mapping_rejects_nonsense() {
        let ok = geom((0, 0), 1.0, (100, 100), (100, 100));
        assert_eq!(map_image_point(&ok, 201, 0), None);
        assert_eq!(map_image_point(&ok, 0, 201), None);
        for g in [
            geom((0, 0), 0.0, (100, 100), (100, 100)),
            geom((0, 0), f32::NAN, (100, 100), (100, 100)),
            geom((0, 0), -1.0, (100, 100), (100, 100)),
            geom((0, 0), 1.0, (0, 100), (100, 100)),
            geom((0, 0), 1.0, (100, 100), (0, 100)),
        ] {
            assert_eq!(map_image_point(&g, 1, 1), None);
        }
    }

    fn displays() -> Vec<DisplayInfo> {
        vec![
            DisplayInfo {
                id: DisplayId(1),
                origin: (0, 0),
                size: (1440, 900),
                scale: 2.0,
            },
            DisplayInfo {
                id: DisplayId(2),
                origin: (-1920, -100),
                size: (1920, 1080),
                scale: 1.0,
            },
        ]
    }

    #[test]
    fn node_mapping_centre_display_and_clamp() {
        let ds = displays();
        let rects = |id: u32| match id {
            1 => Some(Rect::new(100, 100, 40, 20)),
            2 => Some(Rect::new(-1000, 0, 100, 100)),
            3 => Some(Rect::new(-50, -50, 100, 100)), // centre (0,0) is inside window below
            4 => Some(Rect::new(0, 0, 0, 10)),        // empty
            5 => Some(Rect::new(1300, 800, 400, 400)), // centre outside window -> clamp
            6 => Some(Rect::new(10_000, 10_000, 10, 10)),
            _ => None,
        };
        let ctx = NodeContext {
            window_rect: Rect::new(0, 0, 1440, 900),
            displays: &ds,
            node_rect: &rects,
        };
        assert_eq!(map_node_point(&ctx, 1), Some((120, 110, DisplayId(1))));
        assert_eq!(map_node_point(&ctx, 3), Some((0, 0, DisplayId(1))));
        assert_eq!(map_node_point(&ctx, 4), None);
        assert_eq!(map_node_point(&ctx, 5), Some((1439, 899, DisplayId(1))));
        assert_eq!(map_node_point(&ctx, 6), Some((1439, 899, DisplayId(1))));
        assert_eq!(map_node_point(&ctx, 99), None);
        // Window on the left monitor (negative origin).
        let ctx = NodeContext {
            window_rect: Rect::new(-1920, -100, 1920, 1080),
            displays: &ds,
            node_rect: &rects,
        };
        assert_eq!(map_node_point(&ctx, 2), Some((-950, 50, DisplayId(2))));
        // Clamped point on no display -> dropped.
        let ctx = NodeContext {
            window_rect: Rect::new(5000, 5000, 100, 100),
            displays: &ds,
            node_rect: &rects,
        };
        assert_eq!(map_node_point(&ctx, 6), None);
    }

    #[test]
    fn wrong_mode_dropped_silently() {
        let g = geom((0, 0), 1.0, (100, 100), (100, 100));
        let rects = |_| Some(Rect::new(10, 10, 10, 10));
        let ds = displays();
        let node_ctx = || {
            PointContext::Node(NodeContext {
                window_rect: Rect::new(0, 0, 1440, 900),
                displays: &ds,
                node_rect: &rects,
            })
        };
        let image_tag = img(5, 5, "a");
        let node_tag = PointTag::Node {
            id: 1,
            label: "b".into(),
        };
        assert!(map_tag(&image_tag, &PointContext::Image(g)).is_some());
        assert!(map_tag(&node_tag, &PointContext::Image(g)).is_none());
        assert!(map_tag(&node_tag, &node_ctx()).is_some());
        assert!(map_tag(&image_tag, &node_ctx()).is_none());
        assert!(map_tag(&image_tag, &PointContext::None).is_none());
        assert!(map_tag(&node_tag, &PointContext::None).is_none());
        assert_eq!(PointContext::None.mode(), PointMode::None);
        assert_eq!(PointContext::Image(g).mode(), PointMode::Image);
        assert_eq!(node_ctx().mode(), PointMode::Node);
    }

    #[test]
    fn stream_end_to_end_multi_monitor() {
        // Second (left) monitor, 2x scale, downscaled image.
        let g = ImageGeometry {
            origin: (-1440, 0),
            scale: 2.0,
            frame_px: (2880, 1800),
            sent_px: (1440, 900),
            display_id: DisplayId(2),
        };
        let mut s = PointStream::new(PointContext::Image(g));
        let mut text = String::new();
        let mut evs = vec![];
        for d in [
            "Press [POI",
            "NT:720,450:Ok",
            " button:screen2] now [POINT:#1:no",
            "pe] bye",
        ] {
            let (t, e) = s.push(d);
            text.push_str(&t);
            evs.extend(e);
        }
        let (t, e) = s.finish();
        text.push_str(&t);
        evs.extend(e);
        assert_eq!(text, "Press  now  bye");
        assert_eq!(
            evs,
            vec![PointEvent {
                x: -720,
                y: 450,
                label: "Ok button".into(),
                display_id: 2
            }]
        );
    }
}
