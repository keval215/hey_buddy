//! Read results (S2-18): what a `find_text` / `answer_from_screen` step found, as a typed, short
//! [`ReadAnswer`].
//!
//! The ONLY input is the firewall-cleared [`RedactedScreenText`] of the live step's fresh
//! observation (the same text the planner-free read steps are bound to): no pixels, no model, no
//! driver call. `ReadAnswer` has private fields and one constructor, so nothing else can make one.
//! v1 has no model summary: `answer_from_screen` uses the same element-label match as
//! `find_text` (the engine is called once, before any screen is seen, so no model ever reads
//! screen text). The query is the engine's (untrusted) string; it only selects which cleared
//! labels are quoted, and is never quoted itself.
//!
//! The spoken sentence ([`ReadAnswer::sentence`]) is fixed words, a count, a fixed English role
//! word and one cleared label (control characters removed, at most [`MAX_LABEL_CHARS`] chars). It
//! is spoken through the LOCAL TTS slot only. `Debug` prints counts, never the label.

use std::fmt;

use firewall::RedactedScreenText;

/// Most answers one task reports.
pub const MAX_ANSWERS: usize = 4;
/// Longest quoted label.
pub const MAX_LABEL_CHARS: usize = 80;
const MAX_ROLE_CHARS: usize = 24;

/// Which read step produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    FindText,
    AnswerFromScreen,
}

impl ReadKind {
    pub fn code(self) -> &'static str {
        match self {
            ReadKind::FindText => "find_text",
            ReadKind::AnswerFromScreen => "answer_from_screen",
        }
    }
}

/// A short, typed result of one read step.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadAnswer {
    kind: ReadKind,
    matches: u16,
    first_role: String,
    first_label: String,
}

impl ReadAnswer {
    /// Built only from cleared screen text.
    pub fn from_screen(kind: ReadKind, query: &str, text: &RedactedScreenText) -> Self {
        let needle = query.trim().to_lowercase();
        let mut matches = 0u16;
        let mut first: Option<(String, String)> = None;
        if !needle.is_empty() {
            for line in text.text().lines() {
                let Some((role, label)) = element_line(line) else {
                    continue;
                };
                if label.to_lowercase().contains(&needle) {
                    matches = matches.saturating_add(1);
                    if first.is_none() {
                        first = Some((clean_role(role), clean_label(label)));
                    }
                }
            }
        }
        let (first_role, first_label) = first.unwrap_or_default();
        Self {
            kind,
            matches,
            first_role,
            first_label,
        }
    }

    pub fn kind(&self) -> ReadKind {
        self.kind
    }
    pub fn matches(&self) -> u16 {
        self.matches
    }
    /// Fixed English role word of the first match (`button`, `text field`, ...); empty if none.
    pub fn first_role(&self) -> &str {
        &self.first_role
    }
    /// The first matching label, cleaned and shortened; empty if none.
    pub fn first_label(&self) -> &str {
        &self.first_label
    }

    /// The sentence to speak: fixed words around one cleared label.
    pub fn sentence(&self) -> String {
        if self.matches == 0 {
            return "I could not find that on the screen.".to_owned();
        }
        let n = if self.matches == 1 {
            "one match".to_owned()
        } else {
            format!("{} matches", self.matches)
        };
        if self.first_label.is_empty() {
            format!("I found {n}.")
        } else {
            format!(
                "I found {n}. The first is a {}: {}.",
                if self.first_role.is_empty() {
                    "item"
                } else {
                    &self.first_role
                },
                self.first_label
            )
        }
    }
}

impl fmt::Debug for ReadAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ReadAnswer({}, {} matches, label {} chars)",
            self.kind.code(),
            self.matches,
            self.first_label.chars().count()
        )
    }
}

/// `#<n> <role words> "<label>" [flags]` -> (role words, label). Other lines (`app: ...`) are
/// skipped: the app id is never part of an answer.
fn element_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('#')?;
    let (num, rest) = rest.split_once(' ')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let open = rest.find('"')?;
    let close = rest.rfind('"')?;
    if close <= open {
        return None;
    }
    Some((rest[..open].trim(), &rest[open + 1..close]))
}

fn clean_role(role: &str) -> String {
    role.chars()
        .filter(|c| c.is_ascii_alphabetic() || *c == ' ')
        .take(MAX_ROLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn clean_label(label: &str) -> String {
    let s: String = label
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    s.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_lines_parse_and_other_lines_are_skipped() {
        assert_eq!(
            element_line("#3 button \"Sign in\""),
            Some(("button", "Sign in"))
        );
        assert_eq!(
            element_line("#1 text field \"Email\" [has text]"),
            Some(("text field", "Email"))
        );
        assert_eq!(element_line("app: com.example.app"), None);
        assert_eq!(element_line("#2 password field [hidden]"), None);
        assert_eq!(element_line("#x button \"a\""), None);
    }

    #[test]
    fn labels_are_cleaned_and_bounded() {
        let long = "a".repeat(500);
        assert_eq!(clean_label(&long).chars().count(), MAX_LABEL_CHARS);
        assert_eq!(clean_label("  a\u{7}b\n "), "ab");
        assert_eq!(clean_role("but\u{0}ton1"), "button");
    }
}
