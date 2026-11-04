//! Width and confusable folding for the label lexicons (S2-22b, red-team findings F-1b).
//!
//! Shared by the firewall's matcher and `policy`'s copy so the two layers cannot drift. Everything
//! here only ever ADDS matches (a second, folded form of the label is matched IN ADDITION to the
//! plain one), so it can only cause refusals / escalations (D-008).
//!
//! * [`fold_width`] is a small in-house NFKC subset applied to every character before lowercasing:
//!   fullwidth ASCII, mathematical alphanumerics (bold / italic / script ... letters and digits)
//!   and circled letters become plain ASCII. No new dependency.
//! * [`skeleton`] maps Cyrillic / Greek letters that look like Latin letters to the Latin letter,
//!   but ONLY in a word that mixes Latin with such letters ("D\u{435}lete") or consists entirely
//!   of such letters ("\u{440}\u{430}\u{443}", which reads `pay`). Any other word is untouched, so
//!   ordinary Russian / Greek UI text is not rewritten.
//! * [`mixed_script`] reports a word that mixes Latin letters with Cyrillic / Greek ones: a real
//!   label never needs that, so `policy` treats such a click as at least a submit.
//!
//! All functions are linear in the input, never index by byte offset into folded text, and never
//! panic. Inputs are already bounded by the label caps upstream.

/// Fullwidth, mathematical-alphanumeric and circled letters/digits to ASCII; other chars unchanged.
pub fn fold_width(c: char) -> char {
    let cp = c as u32;
    let ascii = |base: u32, off: u32| char::from_u32(base + off);
    let r = match cp {
        // Fullwidth ASCII variants (U+FF01..FF5E map to U+0021..007E).
        0xFF01..=0xFF5E => char::from_u32(cp - 0xFF01 + 0x21),
        // Mathematical alphanumerics: 13 styles x (26 upper + 26 lower), then digit styles.
        0x1D400..=0x1D6A3 => {
            let i = (cp - 0x1D400) % 52;
            if i < 26 {
                ascii(u32::from(b'A'), i)
            } else {
                ascii(u32::from(b'a'), i - 26)
            }
        }
        0x1D7CE..=0x1D7FF => ascii(u32::from(b'0'), (cp - 0x1D7CE) % 10),
        // Circled letters.
        0x24B6..=0x24CF => ascii(u32::from(b'A'), cp - 0x24B6),
        0x24D0..=0x24E9 => ascii(u32::from(b'a'), cp - 0x24D0),
        _ => None,
    };
    r.unwrap_or(c)
}

/// Latin letter that a (lowercase) Cyrillic or Greek letter is commonly confused with.
fn confusable(c: char) -> Option<char> {
    Some(match c {
        // Cyrillic (capital forms that only look Latin when upper-case are included: the label
        // is shown to the user in its original case).
        'а' => 'a',
        'в' => 'b',
        'е' | 'ё' => 'e',
        'к' => 'k',
        'м' => 'm',
        'н' => 'h',
        'о' => 'o',
        'р' => 'p',
        'с' => 'c',
        'т' => 't',
        'у' => 'y',
        'х' => 'x',
        'і' | 'ї' => 'i',
        'ј' => 'j',
        'ѕ' => 's',
        'һ' => 'h',
        'ԁ' => 'd',
        'ԛ' => 'q',
        'ԝ' => 'w',
        'ӏ' => 'l',
        'ѵ' => 'v',
        'ɡ' => 'g',
        // Greek (incl. tonos forms).
        'α' | 'ά' => 'a',
        'β' => 'b',
        'ε' | 'έ' => 'e',
        'ι' | 'ί' | 'ϊ' => 'i',
        'κ' => 'k',
        'μ' => 'u',
        'ν' => 'v',
        'ο' | 'ό' => 'o',
        'ρ' => 'p',
        'τ' => 't',
        'υ' | 'ύ' | 'ϋ' => 'u',
        'χ' => 'x',
        'η' | 'ή' => 'n',
        'ω' | 'ώ' => 'w',
        'ϲ' => 'c',
        'ϳ' => 'j',
        _ => return None,
    })
}

struct Counts {
    latin: usize,
    lookalike: usize,
    other: usize,
}

fn count(word: &str) -> Counts {
    let mut k = Counts {
        latin: 0,
        lookalike: 0,
        other: 0,
    };
    for c in word.chars() {
        if c.is_ascii_alphabetic() {
            k.latin += 1;
        } else if confusable(c).is_some() {
            k.lookalike += 1;
        } else if c.is_alphabetic() {
            k.other += 1;
        }
    }
    k
}

/// A word that mixes Latin letters with Cyrillic / Greek lookalikes.
fn word_is_mixed(word: &str) -> bool {
    let k = count(word);
    k.latin > 0 && k.lookalike > 0
}

/// `norm` is a lexicon-normalised string (lowercase words separated by single spaces, padded).
/// Returns the same string with confusable letters mapped to Latin in mixed words and in words made
/// only of lookalike letters. Same word boundaries, so `" phrase "` matching still works.
pub fn skeleton(norm: &str) -> String {
    let mut out = String::with_capacity(norm.len());
    for (i, word) in norm.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let k = count(word);
        let fold = k.lookalike > 0 && (k.latin > 0 || k.other == 0);
        for c in word.chars() {
            match fold.then(|| confusable(c)).flatten() {
                Some(l) => out.push(l),
                None => out.push(c),
            }
        }
    }
    out
}

/// True when any word of `norm` (lexicon-normalised) mixes Latin with Cyrillic / Greek letters.
pub fn mixed_script(norm: &str) -> bool {
    norm.split(' ').any(word_is_mixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_folds() {
        let s: String = "\u{ff30}\u{ff41}\u{ff59} \u{1d40f}\u{1d41a}\u{24d0}\u{ff11}"
            .chars()
            .map(fold_width)
            .collect();
        assert_eq!(s, "Pay Paa1");
        assert_eq!(fold_width('é'), 'é');
        assert_eq!(fold_width('\u{1d6a3}'), 'z'); // last of the letter range
    }

    #[test]
    fn skeleton_maps_mixed_and_all_lookalike_words_only() {
        assert_eq!(skeleton(" d\u{435}lete "), " delete ");
        assert_eq!(skeleton(" \u{440}\u{430}\u{443} now "), " pay now ");
        // Pure Cyrillic with a non-lookalike letter, pure Greek: untouched.
        assert_eq!(skeleton(" привет "), " привет ");
        assert_eq!(
            skeleton(" \u{3b1}\u{3b2}\u{3b3} "),
            " \u{3b1}\u{3b2}\u{3b3} "
        );
        assert_eq!(skeleton("  "), "  ");
        assert_eq!(skeleton(""), "");
    }

    #[test]
    fn mixed_script_needs_latin_and_a_lookalike_in_one_word() {
        assert!(mixed_script(" d\u{435}lete "));
        assert!(mixed_script(" ok b\u{443}y "));
        assert!(!mixed_script(" delete "));
        assert!(!mixed_script(" привет "));
        assert!(!mixed_script(
            " hello \u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} "
        ));
        assert!(!mixed_script(""));
    }
}
