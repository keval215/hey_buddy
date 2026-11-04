//! The kill-switch shortcut (S2-13): pure validation and chord parsing (Linux-testable). The
//! OS registration (Windows `RegisterHotKey` on its own thread) lives in the desktop shell and
//! uses [`parse_chord`] for the modifier mask and key.
//!
//! Rules: the push-to-talk grammar (Ctrl/Alt/Shift/Super plus exactly one key), AT LEAST TWO
//! modifiers of which one is Ctrl, Alt or Super (so it is hard to hit by accident and never
//! steals plain typing), and never equal to the push-to-talk chord.

use crate::api::IssueCode;
use crate::settings::hotkey_issue;

/// Validates `input` as the kill chord; `ptt` is the push-to-talk chord it must differ from.
/// Returns the canonical text (`Ctrl+Alt+Shift+K`).
pub fn kill_hotkey_issue(input: &str, ptt: &str) -> Result<String, IssueCode> {
    let canon = hotkey_issue(input)?;
    let chord = parse_chord(&canon).ok_or(IssueCode::InvalidFormat)?;
    if chord.modifier_count() < 2 {
        return Err(IssueCode::NeedsTwoModifiers);
    }
    // Compare canonical forms; an unparseable PTT value (never stored) counts as different.
    if hotkey_issue(ptt).is_ok_and(|p| p == canon) {
        return Err(IssueCode::Duplicate);
    }
    Ok(canon)
}

/// A parsed canonical chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
    /// `A`..`Z`, `0`..`9`, `F1`..`F24` or `Space`.
    pub key: String,
}

impl Chord {
    pub fn modifier_count(&self) -> usize {
        [self.ctrl, self.alt, self.shift, self.sup]
            .iter()
            .filter(|b| **b)
            .count()
    }

    /// Windows `RegisterHotKey` modifier mask (MOD_ALT 1, MOD_CONTROL 2, MOD_SHIFT 4, MOD_WIN 8)
    /// plus MOD_NOREPEAT (0x4000).
    pub fn win_modifiers(&self) -> u32 {
        let mut m = 0x4000;
        if self.alt {
            m |= 0x1;
        }
        if self.ctrl {
            m |= 0x2;
        }
        if self.shift {
            m |= 0x4;
        }
        if self.sup {
            m |= 0x8;
        }
        m
    }

    /// Windows virtual-key code of the key.
    pub fn win_vk(&self) -> Option<u32> {
        let k = self.key.as_str();
        if k == "Space" {
            return Some(0x20);
        }
        // An F-key needs at least one digit after the `F`; the bare letter `F` is the letter key.
        if k.len() >= 2 {
            if let Some(d) = k.strip_prefix('F') {
                if !d.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let n: u32 = d.parse().ok()?;
                return (1..=24).contains(&n).then_some(0x70 + n - 1);
            }
        }
        let b = *k.as_bytes().first()?;
        (k.len() == 1 && b.is_ascii_alphanumeric()).then_some(u32::from(b.to_ascii_uppercase()))
    }
}

/// Parses a CANONICAL chord (the output of `hotkey_issue`).
pub fn parse_chord(canon: &str) -> Option<Chord> {
    let mut c = Chord {
        ctrl: false,
        alt: false,
        shift: false,
        sup: false,
        key: String::new(),
    };
    for part in canon.split('+') {
        match part {
            "Ctrl" => c.ctrl = true,
            "Alt" => c.alt = true,
            "Shift" => c.shift = true,
            "Super" => c.sup = true,
            k if c.key.is_empty() && !k.is_empty() => c.key = k.to_string(),
            _ => return None,
        }
    }
    (!c.key.is_empty()).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{DEFAULT_HOTKEY, DEFAULT_KILL_HOTKEY};

    #[test]
    fn validation_table() {
        let ptt = DEFAULT_HOTKEY;
        let ok = [
            ("Ctrl+Alt+Shift+K", "Ctrl+Alt+Shift+K"),
            ("ctrl+alt+k", "Ctrl+Alt+K"),
            ("Alt + Shift + F12", "Alt+Shift+F12"),
            ("cmd+shift+x", "Shift+Super+X"),
            ("Ctrl+Alt+Space", "Ctrl+Alt+Space"), // differs only if PTT differs: see below
        ];
        for (input, want) in ok {
            let got = kill_hotkey_issue(input, "Ctrl+Shift+P");
            assert_eq!(got.as_deref(), Ok(want), "{input}");
        }
        assert_eq!(
            kill_hotkey_issue(DEFAULT_KILL_HOTKEY, ptt).unwrap(),
            DEFAULT_KILL_HOTKEY
        );
        let bad = [
            ("", IssueCode::InvalidFormat),
            ("K", IssueCode::NeedsModifier),
            ("Shift+K", IssueCode::NeedsModifier),
            ("Ctrl+K", IssueCode::NeedsTwoModifiers),
            ("Alt+K", IssueCode::NeedsTwoModifiers),
            ("Super+K", IssueCode::NeedsTwoModifiers),
            ("Ctrl+Ctrl+K", IssueCode::InvalidFormat),
            ("Ctrl+Alt", IssueCode::InvalidFormat),
            ("Ctrl+Alt+K+L", IssueCode::InvalidFormat),
            ("Ctrl+Alt+Escape", IssueCode::InvalidFormat),
            ("Ctrl+Alt+F25", IssueCode::InvalidFormat),
            ("Ctrl+Alt+\u{202e}K", IssueCode::ControlChars),
            ("Ctrl+Alt+\u{0}K", IssueCode::ControlChars),
            ("Ctrl+Alt+K\n", IssueCode::ControlChars),
        ];
        for (input, code) in bad {
            assert_eq!(kill_hotkey_issue(input, ptt), Err(code), "{input:?}");
        }
        // Hostile and huge strings.
        assert_eq!(
            kill_hotkey_issue(&"Ctrl+".repeat(40), ptt),
            Err(IssueCode::TooLong)
        );
        assert!(kill_hotkey_issue("Ctrl+Alt+<script>", ptt).is_err());
        assert!(kill_hotkey_issue("Ctrl+Alt+K; rm -rf /", ptt).is_err());
        assert!(kill_hotkey_issue("../../etc", ptt).is_err());
    }

    #[test]
    fn it_must_differ_from_push_to_talk() {
        assert_eq!(
            kill_hotkey_issue("Ctrl+Alt+Space", "Ctrl+Alt+Space"),
            Err(IssueCode::Duplicate)
        );
        // Order and case do not matter.
        assert_eq!(
            kill_hotkey_issue("space+alt+ctrl", "Ctrl+Alt+Space"),
            Err(IssueCode::Duplicate)
        );
        assert_eq!(
            kill_hotkey_issue("Ctrl+Alt+Shift+K", "alt+shift+ctrl+k"),
            Err(IssueCode::Duplicate)
        );
    }

    #[test]
    fn chord_parsing_and_windows_codes() {
        let c = parse_chord("Ctrl+Alt+Shift+K").unwrap();
        assert_eq!(c.modifier_count(), 3);
        assert_eq!(c.win_modifiers(), 0x4000 | 0x1 | 0x2 | 0x4);
        assert_eq!(c.win_vk(), Some(0x4B));
        assert_eq!(parse_chord("Alt+Super+F1").unwrap().win_vk(), Some(0x70));
        assert_eq!(parse_chord("Alt+Super+F24").unwrap().win_vk(), Some(0x87));
        assert_eq!(parse_chord("Ctrl+Alt+Space").unwrap().win_vk(), Some(0x20));
        assert_eq!(parse_chord("Ctrl+Alt+7").unwrap().win_vk(), Some(0x37));
        assert!(parse_chord("Ctrl+Alt").is_none()); // no key
    }

    fn vk_of(key: &str) -> Option<Chord> {
        Some(Chord {
            ctrl: true,
            alt: true,
            shift: false,
            sup: false,
            key: key.to_string(),
        })
    }

    #[test]
    fn the_letter_f_is_not_an_f_key() {
        assert_eq!(parse_chord("Ctrl+Alt+F").unwrap().win_vk(), Some(0x46));
        assert_eq!(
            kill_hotkey_issue("Ctrl+Alt+F", "Ctrl+Shift+P").unwrap(),
            "Ctrl+Alt+F"
        );
        for n in 1..=24u32 {
            let c = parse_chord(&format!("Ctrl+Alt+F{n}")).unwrap();
            assert_eq!(c.win_vk(), Some(0x70 + n - 1), "F{n}");
        }
        for bad in ["F0", "F25", "Fx", "FF", "F+", "F-1", "F1x", ""] {
            assert_eq!(vk_of(bad).unwrap().win_vk(), None, "{bad:?}");
        }
        for bad in [
            "Ctrl+Alt+F0",
            "Ctrl+Alt+F25",
            "Ctrl+Alt+Fx",
            "Ctrl+Alt+FF",
            "Ctrl+Alt+",
        ] {
            assert!(kill_hotkey_issue(bad, "Ctrl+Shift+P").is_err(), "{bad}");
        }
    }

    #[test]
    fn validation_and_windows_codes_agree() {
        let mut keys: Vec<String> = ('A'..='Z').chain('0'..='9').map(String::from).collect();
        keys.extend((1..=24).map(|n| format!("F{n}")));
        keys.push("Space".into());
        for k in &keys {
            let input = format!("Ctrl+Alt+{k}");
            let canon = kill_hotkey_issue(&input, "Ctrl+Shift+P")
                .unwrap_or_else(|e| panic!("{input}: {e:?}"));
            let vk = parse_chord(&canon).unwrap().win_vk();
            let want = match k.as_str() {
                "Space" => Some(0x20),
                f if f.len() > 1 && f.starts_with('F') => {
                    Some(0x70 + f[1..].parse::<u32>().unwrap() - 1)
                }
                one => Some(u32::from(one.as_bytes()[0])),
            };
            assert_eq!(vk, want, "{k}");
        }
        // Every other key the validator might ever accept must also have a VK: probe widely.
        let probes = [
            "Escape", "Enter", "Tab", "Left", "Delete", "F", "f", "f1", "F01", "F100", "Fx", "-",
            "é", "ａ", "٣", "F٣",
        ];
        for k in probes {
            if let Ok(canon) = kill_hotkey_issue(&format!("Ctrl+Alt+{k}"), "Ctrl+Shift+P") {
                assert!(
                    parse_chord(&canon).unwrap().win_vk().is_some(),
                    "validation accepted {k:?} but it has no Windows VK"
                );
            }
        }
    }
}
