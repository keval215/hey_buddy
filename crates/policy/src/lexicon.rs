//! English-only label lexicons (ADR 0003 section 5 rows 9, 10, 11, 14). They only ESCALATE: a
//! match raises a tier, a miss never lowers one (labels in other languages fall to the role
//! baseline, a documented limitation).
//!
//! Matching: invisible/format characters stripped (not turned into spaces), case-folded,
//! accents folded, every run of non-alphanumerics one space, then whole-word phrase match.

use crate::norm;

pub const SUBMIT: &[&str] = &[
    "submit", "send", "post", "publish", "reply", "save", "sign in", "signin", "confirm", "ok",
    "continue", "next", "apply",
];

pub const DESTRUCTIVE: &[&str] = &[
    "delete",
    "remove",
    "erase",
    "uninstall",
    "format",
    "reset",
    "wipe",
    "empty",
    "close account",
    "discard",
];

pub const PURCHASE: &[&str] = &[
    "buy",
    "pay",
    "checkout",
    "check out",
    "place order",
    "subscribe",
    "donate",
    "transfer",
    "send money",
    "top up",
];

/// Row 14 payment/secret words (the firewall's own `sensitive_label` flag is also honoured).
pub const SECRET: &[&str] = &[
    "password",
    "passwd",
    "passcode",
    "pin",
    "cvv",
    "cvv2",
    "cvc",
    "cvc2",
    "card number",
    "iban",
    "otp",
    "security code",
];

fn matches(label: &str, list: &[&str]) -> bool {
    let n = norm::words(label);
    list.iter().any(|p| n.contains(&format!(" {p} ")))
}

pub fn is_submit(label: &str) -> bool {
    matches(label, SUBMIT)
}
pub fn is_destructive(label: &str) -> bool {
    matches(label, DESTRUCTIVE)
}
pub fn is_purchase(label: &str) -> bool {
    matches(label, PURCHASE)
}
pub fn is_secret(label: &str) -> bool {
    matches(label, SECRET)
}

/// Starts of the common Unicode decimal-digit blocks (each block is ten consecutive code points
/// for 0-9). Folding them stops fullwidth / Arabic-Indic / Devanagari ... digits hiding a number.
const DIGIT_BLOCKS: &[u32] = &[
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
    0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6,
];

/// Value 0-9 of an ASCII or (common) Unicode decimal digit.
fn digit_value(c: char) -> Option<u8> {
    if let Some(d) = c.to_digit(10) {
        return Some(d as u8);
    }
    let cp = c as u32;
    DIGIT_BLOCKS
        .iter()
        .find(|s| (**s..**s + 10).contains(&cp))
        .map(|s| (cp - s) as u8)
}

/// Characters allowed between the digits of a number.
fn is_digit_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '-' | '.' | '/' | '_' | '\u{2010}'..='\u{2015}' | '\u{2212}'
        )
}

/// Text containing 13-19 digits (Unicode decimal digits folded to 0-9; whitespace incl. NBSP and
/// `- . / _` allowed between digits) that pass the Luhn check. A digit run of 13-19 is tested
/// whole; a longer run is tested in every 13-19 window so padding cannot hide a number.
pub fn looks_like_card_number(text: &str) -> bool {
    let mut runs: Vec<Vec<u8>> = vec![Vec::new()];
    for c in text.chars() {
        if let Some(d) = digit_value(c) {
            if let Some(r) = runs.last_mut() {
                r.push(d);
            }
        } else if !is_digit_separator(c) {
            runs.push(Vec::new());
        }
    }
    runs.iter().any(|r| match r.len() {
        13..=19 => luhn(r),
        n if n > 19 => (13..=19).any(|w| r.windows(w).any(luhn)),
        _ => false,
    })
}

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (i, d) in digits.iter().rev().enumerate() {
        let mut v = u32::from(*d);
        if i % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(l: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = l.iter().map(|s| (*s).to_string()).collect();
        v.sort();
        v.dedup();
        v
    }

    fn only_in(a: &[&str], b: &[&str]) -> Vec<String> {
        sorted(a)
            .into_iter()
            .filter(|w| !b.contains(&w.as_str()))
            .collect()
    }

    /// DRIFT TEST. The firewall evaluates its lexicons on the joined label and policy keeps its
    /// own copy as defence in depth (both are ORed). The submit / destructive / purchase lists
    /// must stay IDENTICAL: a word added to only one side silently weakens the other layer.
    #[test]
    fn submit_destructive_purchase_lists_match_the_firewall() {
        for (name, policy, fw) in [
            ("submit", SUBMIT, firewall::SUBMIT_LEXICON),
            ("destructive", DESTRUCTIVE, firewall::DESTRUCTIVE_LEXICON),
            ("purchase", PURCHASE, firewall::PURCHASE_LEXICON),
        ] {
            assert_eq!(
                only_in(policy, fw),
                Vec::<String>::new(),
                "{name}: words only in policy (add them to firewall/src/sensitive.rs)"
            );
            assert_eq!(
                only_in(fw, policy),
                Vec::<String>::new(),
                "{name}: words only in the firewall (add them to policy/src/lexicon.rs)"
            );
        }
    }

    /// The secret lists legitimately differ: the firewall's `SECRET` is the broad one (it marks a
    /// TEXT TARGET `sensitive_label`: tokens, keys, recovery phrases, 2FA wording, German and
    /// short forms), while policy's `SECRET` is the row-14 tier escalation and adds only the
    /// payment words that the firewall keeps in its separate `PAYMENT` list (which flags the
    /// SCREEN). Both are ORed, so each side only ever adds refusals. The exact differences are
    /// PINNED here: changing either list fails this test until the difference is reviewed and
    /// updated on purpose.
    #[test]
    fn secret_lists_differ_only_in_the_pinned_words() {
        assert_eq!(
            only_in(SECRET, firewall::SECRET_LEXICON),
            ["card number", "cvc2", "cvv2", "iban"]
        );
        assert_eq!(
            only_in(firewall::SECRET_LEXICON, SECRET),
            [
                "2 step",
                "2fa",
                "access key",
                "access token",
                "api key",
                "apikey",
                "auth code",
                "auth token",
                "authentication code",
                "authenticator code",
                "backup code",
                "bearer",
                "card num",
                "ccv",
                "mfa",
                "mnemonic",
                "one time code",
                "one time passcode",
                "one time password",
                "pass code",
                "passphrase",
                "passwort",
                "pin code",
                "pin number",
                "private key",
                "pwd",
                "recovery code",
                "recovery phrase",
                "secret",
                "secret key",
                "secret phrase",
                "seed phrase",
                "social security",
                "ssn",
                "token",
                "two factor",
                "two step",
                "verification code"
            ]
        );
        // What policy's extra words cover on the firewall side: payment words flag the screen.
        for w in ["card number", "cvv2", "cvc2", "iban"] {
            assert!(
                firewall::PAYMENT_LEXICON.contains(&w),
                "{w} must stay in the firewall PAYMENT list"
            );
        }
        // Shared core secrets are in both.
        for w in ["password", "passwd", "passcode", "cvv", "cvc"] {
            assert!(
                SECRET.contains(&w) && firewall::SECRET_LEXICON.contains(&w),
                "{w}"
            );
        }
    }
}
