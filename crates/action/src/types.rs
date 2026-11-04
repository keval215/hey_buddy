use std::fmt;

use crate::error::ActionError;

pub const MAX_TYPED_TEXT_CHARS: usize = 1000;
pub const MAX_LAUNCH_ID_CHARS: usize = 64;

/// Text the agent may type. 1..=1000 chars. Every character for which
/// [`firewall::is_hostile_char`] is true is REJECTED (never stripped or rewritten), so newlines,
/// tabs, bidi and format characters, tags and private use never get through. Enter is only
/// available as `KeyCombo::Enter`. No `Display`; `Debug` prints the length only.
#[derive(Clone, PartialEq, Eq)]
pub struct TypedText(String);

impl TypedText {
    pub fn new(raw: &str) -> Result<Self, ActionError> {
        let mut n = 0usize;
        for c in raw.chars() {
            if firewall::is_hostile_char(c) {
                return Err(ActionError::TextHostileChar);
            }
            n += 1;
            if n > MAX_TYPED_TEXT_CHARS {
                return Err(ActionError::TextTooLong);
            }
        }
        if n == 0 {
            return Err(ActionError::TextEmpty);
        }
        Ok(Self(raw.to_owned()))
    }

    /// For the driver's actuator only; do not log.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Length in characters (1..=1000).
    pub fn char_len(&self) -> u32 {
        self.0.chars().count() as u32
    }
}

impl fmt::Debug for TypedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TypedText(len {})", self.char_len())
    }
}

/// Key into the launch allowlist: `[a-z0-9._-]{1,64}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaunchId(String);

impl LaunchId {
    pub fn new(raw: &str) -> Result<Self, ActionError> {
        let ok = !raw.is_empty()
            && raw.len() <= MAX_LAUNCH_ID_CHARS
            && raw
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(ActionError::LaunchIdInvalid)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScrollDirection {
    Up = 1,
    Down = 2,
    Left = 3,
    Right = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScrollAmount {
    SmallStep = 1,
    LargePage = 2,
}

/// Closed allowlist of key combos (ADR 0004 §7). Discriminants are part of the canonical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyCombo {
    Enter = 1,
    Escape = 2,
    Tab = 3,
    ShiftTab = 4,
    Up = 5,
    Down = 6,
    Left = 7,
    Right = 8,
    Home = 9,
    End = 10,
    PageUp = 11,
    PageDown = 12,
    Space = 13,
    Backspace = 14,
    Delete = 15,
    SelectAll = 16,
    Undo = 17,
    Find = 18,
}

impl KeyCombo {
    /// Stable, human-readable chord name (audit/approval UI).
    pub fn chord_name(self) -> &'static str {
        match self {
            KeyCombo::Enter => "Enter",
            KeyCombo::Escape => "Esc",
            KeyCombo::Tab => "Tab",
            KeyCombo::ShiftTab => "Shift+Tab",
            KeyCombo::Up => "Up",
            KeyCombo::Down => "Down",
            KeyCombo::Left => "Left",
            KeyCombo::Right => "Right",
            KeyCombo::Home => "Home",
            KeyCombo::End => "End",
            KeyCombo::PageUp => "PageUp",
            KeyCombo::PageDown => "PageDown",
            KeyCombo::Space => "Space",
            KeyCombo::Backspace => "Backspace",
            KeyCombo::Delete => "Delete",
            KeyCombo::SelectAll => "Ctrl+A",
            KeyCombo::Undo => "Ctrl+Z",
            KeyCombo::Find => "Ctrl+F",
        }
    }

    /// Fact: moves focus/viewport only (arrows, PgUp/PgDn, Home/End, Tab, Shift+Tab, Esc).
    pub fn is_navigation_only(self) -> bool {
        matches!(
            self,
            KeyCombo::Escape
                | KeyCombo::Tab
                | KeyCombo::ShiftTab
                | KeyCombo::Up
                | KeyCombo::Down
                | KeyCombo::Left
                | KeyCombo::Right
                | KeyCombo::Home
                | KeyCombo::End
                | KeyCombo::PageUp
                | KeyCombo::PageDown
        )
    }

    /// Fact: only meaningful (and only allowed) on an editable target.
    pub fn needs_editable_target(self) -> bool {
        matches!(self, KeyCombo::Backspace | KeyCombo::Delete)
    }
}
