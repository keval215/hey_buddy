//! Key chords as they appear in `key_combo{keys:[...]}` (ADR 0003 section 4).
//!
//! The wire vocabulary is closed: modifiers `ctrl alt shift win` and a fixed list of keys. A chord
//! that equals one of `action::KeyCombo`'s chords (or Ctrl+C = `copy_selection`) is folded into
//! that typed step; every other chord stays representable as an [`UnknownChord`] so the user sees
//! it refused, but it can never become an `action::Action`.

use action::KeyCombo;

pub(crate) const CTRL: u8 = 1;
pub(crate) const ALT: u8 = 2;
pub(crate) const SHIFT: u8 = 4;
pub(crate) const WIN: u8 = 8;

const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const FKEYS: [&str; 12] = [
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
];

/// `Some(mask)` for a modifier token.
fn modifier(tok: &str) -> Option<u8> {
    match tok {
        "ctrl" | "control" => Some(CTRL),
        "alt" => Some(ALT),
        "shift" => Some(SHIFT),
        "win" | "windows" | "meta" | "super" | "lwin" | "rwin" => Some(WIN),
        _ => None,
    }
}

/// Canonical static name of a non-modifier key token.
fn key(tok: &str) -> Option<&'static str> {
    Some(match tok {
        "enter" | "return" => "enter",
        "esc" | "escape" => "escape",
        "tab" => "tab",
        "up" => "up",
        "down" => "down",
        "left" => "left",
        "right" => "right",
        "home" => "home",
        "end" => "end",
        "pageup" | "pgup" => "pageup",
        "pagedown" | "pgdn" => "pagedown",
        "space" => "space",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "insert" | "ins" => "insert",
        "printscreen" | "prtsc" => "printscreen",
        t => {
            if let Some(i) = LETTERS.iter().position(|k| *k == t) {
                LETTERS[i]
            } else if let Some(i) = DIGITS.iter().position(|k| *k == t) {
                DIGITS[i]
            } else {
                FKEYS[FKEYS.iter().position(|k| *k == t)?]
            }
        }
    })
}

/// A modifier set plus at most one key, in canonical form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnknownChord {
    mods: u8,
    key: Option<&'static str>,
}

/// What a `keys` array means.
pub(crate) enum Parsed {
    Combo(KeyCombo),
    Copy,
    Chord(UnknownChord),
}

impl UnknownChord {
    /// Parses lowercase tokens. Order-insensitive; duplicates, a second key, unknown tokens and
    /// empty input are errors (`None`).
    pub(crate) fn parse_tokens(tokens: &[String]) -> Option<Parsed> {
        if tokens.is_empty() || tokens.len() > 4 {
            return None;
        }
        let mut mods = 0u8;
        let mut k: Option<&'static str> = None;
        for t in tokens {
            if let Some(m) = modifier(t) {
                if mods & m != 0 {
                    return None;
                }
                mods |= m;
            } else {
                let name = key(t)?;
                if k.is_some() {
                    return None;
                }
                k = Some(name);
            }
        }
        Some(match (mods, k) {
            (0, Some("enter")) => Parsed::Combo(KeyCombo::Enter),
            (0, Some("escape")) => Parsed::Combo(KeyCombo::Escape),
            (0, Some("tab")) => Parsed::Combo(KeyCombo::Tab),
            (SHIFT, Some("tab")) => Parsed::Combo(KeyCombo::ShiftTab),
            (0, Some("up")) => Parsed::Combo(KeyCombo::Up),
            (0, Some("down")) => Parsed::Combo(KeyCombo::Down),
            (0, Some("left")) => Parsed::Combo(KeyCombo::Left),
            (0, Some("right")) => Parsed::Combo(KeyCombo::Right),
            (0, Some("home")) => Parsed::Combo(KeyCombo::Home),
            (0, Some("end")) => Parsed::Combo(KeyCombo::End),
            (0, Some("pageup")) => Parsed::Combo(KeyCombo::PageUp),
            (0, Some("pagedown")) => Parsed::Combo(KeyCombo::PageDown),
            (0, Some("space")) => Parsed::Combo(KeyCombo::Space),
            (0, Some("backspace")) => Parsed::Combo(KeyCombo::Backspace),
            (0, Some("delete")) => Parsed::Combo(KeyCombo::Delete),
            (CTRL, Some("a")) => Parsed::Combo(KeyCombo::SelectAll),
            (CTRL, Some("z")) => Parsed::Combo(KeyCombo::Undo),
            (CTRL, Some("f")) => Parsed::Combo(KeyCombo::Find),
            (CTRL, Some("c")) => Parsed::Copy,
            _ => Parsed::Chord(UnknownChord { mods, key: k }),
        })
    }

    /// Parses key names as they appear on the wire. `None` for an invalid vocabulary or for a
    /// chord that is one of the closed typed combos (those are not "unknown").
    pub fn parse(names: &[&str]) -> Option<Self> {
        let toks: Vec<String> = names.iter().map(|s| s.to_lowercase()).collect();
        match Self::parse_tokens(&toks)? {
            Parsed::Chord(c) => Some(c),
            _ => None,
        }
    }

    pub(crate) fn has_win(&self) -> bool {
        self.mods & WIN != 0
    }

    /// `ctrl+alt+delete` style comparison.
    pub(crate) fn is(&self, mods: u8, key: &str) -> bool {
        self.mods == mods && self.key == Some(key)
    }

    /// The key matches whatever the modifiers are.
    pub(crate) fn key_is(&self, key: &str) -> bool {
        self.key == Some(key)
    }

    /// Canonical human-readable name, e.g. `Ctrl+Alt+delete` (modifiers in fixed order).
    pub fn name(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.mods & CTRL != 0 {
            parts.push("Ctrl");
        }
        if self.mods & ALT != 0 {
            parts.push("Alt");
        }
        if self.mods & SHIFT != 0 {
            parts.push("Shift");
        }
        if self.mods & WIN != 0 {
            parts.push("Win");
        }
        if let Some(k) = self.key {
            parts.push(k);
        }
        parts.join("+")
    }

    pub(crate) fn canonical(&self, out: &mut Vec<u8>) {
        out.push(self.mods);
        let k = self.key.unwrap_or("");
        out.extend_from_slice(&(k.len() as u64).to_le_bytes());
        out.extend_from_slice(k.as_bytes());
    }
}

impl std::fmt::Debug for UnknownChord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnknownChord({})", self.name())
    }
}
