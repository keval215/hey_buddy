//! Human-readable approval summaries built ONLY from validated, typed data (S2-16).
//!
//! The approval window must never show model prose. Every line is a fixed template over typed
//! fields: an operation token (closed enum), a tier, and at most two untrusted strings (the target
//! label and, for typing, the exact text). Untrusted strings go through [`clean`]: control, bidi,
//! zero-width, tag and other invisible characters are removed (whitespace controls become a
//! space, runs of whitespace collapse), `<` `>` become angle quotes, `"` becomes `'`, and the
//! result is cut at a fixed length. The webview renders them as text nodes only, so this is a
//! second line of defence, not the only one.
//!
//! Debug of the input types never prints labels or text (L-022).

use std::fmt;

use firewall::is_hostile_char;
use policy::Tier;

use super::dto::{ApprovalLineDto, ApprovalTierDto};

/// Longest label shown (ADR 0003 section 7: capped at 60 characters).
pub const MAX_LABEL_CHARS: usize = 60;
/// Longest typed text shown in full (matches the plan's `type.text` cap).
pub const MAX_TEXT_CHARS: usize = 1000;
/// Most steps one plan approval lists (the plan itself has at most 12).
pub const MAX_LINES: usize = 12;

/// The abstract operation (closed set; mirrors the plan vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Observe,
    FindText,
    AnswerFromScreen,
    Scroll,
    FocusWindow,
    OpenApp,
    Click,
    Type,
    Submit,
    KeyCombo,
    CopySelection,
    /// A chord outside the closed key vocabulary (always refused, shown so the user sees it).
    KeyChord,
}

impl OpKind {
    /// Fixed token shown in the window (never derived from input).
    pub fn token(self) -> &'static str {
        match self {
            OpKind::Observe => "observe",
            OpKind::FindText => "find_text",
            OpKind::AnswerFromScreen => "answer_from_screen",
            OpKind::Scroll => "scroll",
            OpKind::FocusWindow => "focus_window",
            OpKind::OpenApp => "open_app",
            OpKind::Click => "click",
            OpKind::Type => "type",
            OpKind::Submit => "submit",
            OpKind::KeyCombo => "key_combo",
            OpKind::CopySelection => "copy_selection",
            OpKind::KeyChord => "key_chord",
        }
    }
}

/// One step as the policy layer knows it: typed facts only.
#[derive(Clone)]
pub struct StepFacts {
    pub op: OpKind,
    /// Element label or app name (untrusted text, shown as inert text).
    pub target: Option<String>,
    /// The exact text a `type` step enters (untrusted, shown in full up to the cap).
    pub text: Option<String>,
    pub tier: Tier,
    /// Not covered by a plan approval: will ask again.
    pub ask_again: bool,
}

impl StepFacts {
    pub fn new(op: OpKind, tier: Tier) -> Self {
        Self {
            op,
            target: None,
            text: None,
            tier,
            ask_again: false,
        }
    }
    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = Some(t.into());
        self
    }
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = Some(t.into());
        self
    }
    pub fn ask_again(mut self, v: bool) -> Self {
        self.ask_again = v;
        self
    }
}

impl fmt::Debug for StepFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StepFacts")
            .field("op", &self.op)
            .field("tier", &self.tier)
            .field("has_target", &self.target.is_some())
            .field("has_text", &self.text.is_some())
            .finish()
    }
}

/// Strip, neutralise and cut `s` to `max` characters (an ellipsis counts toward the cap).
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    let mut n = 0usize;
    let mut truncated = false;
    for c in s.chars() {
        let c = match c {
            c if c.is_whitespace() => ' ',
            // Whitespace-like controls (tab, newline, CR ...) were caught above; the rest of the
            // hostile set (bidi, zero width, tags, other controls) is dropped.
            c if is_hostile_char(c) => continue,
            '<' => '\u{2039}',
            '>' => '\u{203A}',
            '"' => '\'',
            c => c,
        };
        if c == ' ' {
            pending_space = !out.is_empty();
            continue;
        }
        let need = 1 + usize::from(pending_space);
        if n + need > max {
            truncated = true;
            break;
        }
        if pending_space {
            out.push(' ');
            n += 1;
            pending_space = false;
        }
        out.push(c);
        n += 1;
    }
    if truncated {
        // Make room for the ellipsis inside the cap.
        while n >= max && !out.is_empty() {
            out.pop();
            n -= 1;
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\u{2026}');
    }
    out
}

fn tier_dto(t: Tier) -> (ApprovalTierDto, bool) {
    match t {
        Tier::Read => (ApprovalTierDto::Read, false),
        Tier::Reversible => (ApprovalTierDto::Reversible, false),
        Tier::Consequential => (ApprovalTierDto::Consequential, false),
        // A Never step is shown as refused; it never runs.
        Tier::Never => (ApprovalTierDto::Consequential, true),
    }
}

/// One step as a display line.
pub fn line(index: usize, s: &StepFacts) -> ApprovalLineDto {
    let (tier, refused) = tier_dto(s.tier);
    ApprovalLineDto {
        index: u16::try_from(index).unwrap_or(u16::MAX),
        op: s.op.token().to_string(),
        target: s.target.as_deref().map(|t| clean(t, MAX_LABEL_CHARS)),
        text: s.text.as_deref().map(|t| clean(t, MAX_TEXT_CHARS)),
        tier,
        refused,
        ask_again: s.ask_again && !refused,
    }
}

/// Lines of a plan (at most [`MAX_LINES`]; the plan layer already caps at 12).
pub fn plan_lines(steps: &[StepFacts]) -> Vec<ApprovalLineDto> {
    steps
        .iter()
        .take(MAX_LINES)
        .enumerate()
        .map(|(i, s)| line(i, s))
        .collect()
}

/// Fixed-format one-line summary of a plan: numbers and tokens only.
pub fn plan_summary(digest8: &str, lines: &[ApprovalLineDto]) -> String {
    let count = |f: &dyn Fn(&ApprovalLineDto) -> bool| lines.iter().filter(|l| f(l)).count();
    let read = count(&|l| !l.refused && l.tier == ApprovalTierDto::Read);
    let rev = count(&|l| !l.refused && l.tier == ApprovalTierDto::Reversible);
    let cons = count(&|l| !l.refused && l.tier == ApprovalTierDto::Consequential);
    let refused = count(&|l| l.refused);
    format!(
        "Plan {}: {} steps ({} read, {} reversible, {} consequential, {} refused)",
        hex_only(digest8),
        lines.len(),
        read,
        rev,
        cons,
        refused
    )
}

/// Fixed-format one-line summary of the "Run it for real?" offer that follows a dry run (S2-23).
/// Distinct from [`plan_summary`] so the window can tell the two questions apart.
pub fn live_offer_summary(digest8: &str, lines: &[ApprovalLineDto]) -> String {
    format!("Run it for real? {}", plan_summary(digest8, lines))
}

/// Fixed-format one-line summary of a step approval.
pub fn step_summary(line: &ApprovalLineDto, submit: bool, changed: bool, digest8: &str) -> String {
    let tier = match line.tier {
        ApprovalTierDto::Read => "read",
        ApprovalTierDto::Reversible => "reversible",
        ApprovalTierDto::Consequential => "consequential",
    };
    format!(
        "Step {}: {} ({}{}{}) {}",
        u32::from(line.index) + 1,
        line.op,
        tier,
        if submit { ", submit" } else { "" },
        if changed {
            ", changed since approved"
        } else {
            ""
        },
        hex_only(digest8)
    )
}

/// Lowercase hex digits only (a digest short form can never carry anything else).
fn hex_only(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_override_and_zero_width_are_removed() {
        let s = clean("pay\u{202E}dne\u{200B}d\u{2066}x\u{2069}\u{FEFF}!", 60);
        assert_eq!(s, "paydnedx!");
        assert!(!s.chars().any(is_hostile_char));
    }

    #[test]
    fn newlines_and_tabs_become_one_space() {
        assert_eq!(clean("a\r\n\n\tb\u{2028}c  \u{00A0} d", 60), "a b c d");
        assert_eq!(clean("\n\n  hi  \n", 60), "hi");
    }

    #[test]
    fn script_tags_and_quotes_are_neutralised() {
        let s = clean("<script>alert(\"x\")</script><img onerror=x>", 120);
        assert!(!s.contains('<') && !s.contains('>') && !s.contains('"'));
        assert!(s.contains("script"));
    }

    #[test]
    fn tag_characters_and_controls_are_removed() {
        let s = clean("a\u{E0041}\u{E007F}b\u{0007}c\u{0000}d\u{E000}e", 60);
        assert_eq!(s, "abcde");
    }

    #[test]
    fn huge_text_is_cut_at_the_fixed_length() {
        let big = "x".repeat(100_000);
        let s = clean(&big, MAX_LABEL_CHARS);
        assert_eq!(s.chars().count(), MAX_LABEL_CHARS);
        assert!(s.ends_with('\u{2026}'));
        let exact = "y".repeat(MAX_LABEL_CHARS);
        assert_eq!(clean(&exact, MAX_LABEL_CHARS), exact);
        let t = clean(&big, MAX_TEXT_CHARS);
        assert_eq!(t.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn cut_never_leaves_trailing_space_or_exceeds_cap() {
        let s = clean(&"ab ".repeat(100), 10);
        assert!(s.chars().count() <= 10);
        assert!(!s.trim_end_matches('\u{2026}').ends_with(' '));
    }

    #[test]
    fn lines_use_fixed_tokens_and_clean_labels() {
        let facts = StepFacts::new(OpKind::Click, Tier::Consequential)
            .target("Send\u{202E}\n<b>now</b>")
            .ask_again(true);
        let l = line(2, &facts);
        assert_eq!(l.op, "click");
        assert_eq!(l.index, 2);
        assert_eq!(
            l.target.as_deref(),
            Some("Send \u{2039}b\u{203A}now\u{2039}/b\u{203A}")
        );
        assert!(l.ask_again && !l.refused);
    }

    #[test]
    fn never_steps_are_shown_as_refused_not_as_a_tier_that_runs() {
        let l = line(
            0,
            &StepFacts::new(OpKind::Type, Tier::Never).ask_again(true),
        );
        assert!(l.refused && !l.ask_again);
    }

    #[test]
    fn plan_summary_is_numbers_and_tokens() {
        let steps = vec![
            StepFacts::new(OpKind::Observe, Tier::Read),
            StepFacts::new(OpKind::Scroll, Tier::Reversible),
            StepFacts::new(OpKind::Click, Tier::Consequential).target("<script>"),
            StepFacts::new(OpKind::Type, Tier::Never).target("Password"),
        ];
        let lines = plan_lines(&steps);
        let s = plan_summary("ABCDEF12zz<", &lines);
        assert_eq!(
            s,
            "Plan abcdef12: 4 steps (1 read, 1 reversible, 1 consequential, 1 refused)"
        );
        assert!(!s.contains("script") && !s.contains("Password"));
    }

    #[test]
    fn step_summary_has_no_label_text() {
        let l = line(
            3,
            &StepFacts::new(OpKind::Submit, Tier::Consequential).target("\u{202E}evil"),
        );
        let s = step_summary(&l, true, true, "0123abcd");
        assert_eq!(
            s,
            "Step 4: submit (consequential, submit, changed since approved) 0123abcd"
        );
    }

    #[test]
    fn plans_are_capped_at_the_line_limit() {
        let steps: Vec<_> = (0..40)
            .map(|_| StepFacts::new(OpKind::Observe, Tier::Read))
            .collect();
        assert_eq!(plan_lines(&steps).len(), MAX_LINES);
    }

    #[test]
    fn debug_hides_content() {
        let f = StepFacts::new(OpKind::Type, Tier::Consequential)
            .target("PRIVATE_MARKER_LABEL")
            .text("PRIVATE_MARKER_TEXT");
        let d = format!("{f:?}");
        assert!(d.contains("StepFacts") && !d.contains("PRIVATE_MARKER"));
    }
}
