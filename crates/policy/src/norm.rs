//! Text normalisation shared by provenance and the lexicons.
//!
//! Invisible characters (format, bidi, tags, variation selectors ...: `firewall::is_hostile_char`
//! and not whitespace) are STRIPPED, not turned into spaces: `Del<ZWSP>ete` must read `delete`
//! (S2-14a lesson). Whitespace-like controls (`\n`, `\t`, U+2028) stay separators.

use firewall::is_hostile_char;

/// Pushes the lowercase, accent-folded form of `c`.
fn fold_into(c: char, out: &mut String) {
    for l in c.to_lowercase() {
        match l {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => out.push('i'),
            'ł' => out.push('l'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'ř' => out.push('r'),
            'ś' | 'š' | 'ş' => out.push('s'),
            'ť' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            // Combining diacritical marks (decomposed accents) vanish.
            '\u{0300}'..='\u{036F}' => {}
            x => out.push(x),
        }
    }
}

fn invisible(c: char) -> bool {
    is_hostile_char(c) && !c.is_whitespace()
}

/// Provenance form: invisibles stripped, lowercase, accents folded, whitespace runs collapsed to
/// one space, trimmed. Punctuation is kept (typed text may contain it).
pub(crate) fn provenance(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        fold_into(c, &mut out);
    }
    out
}

/// Lexicon form: as [`provenance`], but every run of non-alphanumerics is one space, and the
/// result is padded with a space on both sides so `" phrase "` is a whole-word match.
pub(crate) fn words(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    for c in s.chars().map(firewall::fold_width) {
        if invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            folded.push(' ');
        } else {
            fold_into(c, &mut folded);
        }
    }
    let mut out = String::with_capacity(folded.len() + 2);
    out.push(' ');
    let mut last_space = true;
    for c in folded.chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !last_space {
        out.push(' ');
    }
    out
}

/// Whole-word containment of `needle` in `hay`, both in [`provenance`] form: if the needle's
/// first (last) character is alphanumeric, the character before (after) the match must not be.
/// An empty needle is never contained.
pub(crate) fn contains_bounded(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let first = needle.chars().next().is_some_and(char::is_alphanumeric);
    let last = needle.chars().last().is_some_and(char::is_alphanumeric);
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = !first
            || hay[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = !last
            || hay[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        // Advance by one character (keep char boundaries).
        from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
        if from >= hay.len() {
            break;
        }
    }
    false
}

/// Sanitise a label/query for DISPLAY and matching: same mapping as the firewall's screen-text
/// sanitiser (`<`/`>` become `‹`/`›`, `"` becomes `'`, hostile characters become spaces,
/// whitespace collapsed). Callers reject hostile input BEFORE this, so for plan strings the
/// hostile arm is only defence in depth.
pub(crate) fn sanitise(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '<' => '\u{2039}',
            '>' => '\u{203A}',
            '"' => '\'',
            c if is_hostile_char(c) => ' ',
            c => c,
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// As [`sanitise`] but invisible characters are REMOVED instead of becoming spaces, so a label
/// split by a zero-width character still reads as one word to the lexicons (S2-14a lesson).
pub(crate) fn sanitise_joined(s: &str) -> String {
    let kept: String = s.chars().filter(|&c| !invisible(c)).collect();
    sanitise(&kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_folds() {
        assert_eq!(provenance("  Café\tNOTEPAD \n x "), "cafe notepad x");
        assert_eq!(provenance("Stra\u{00DF}e"), "strasse");
        assert_eq!(provenance("e\u{0301}"), "e");
        assert_eq!(provenance("a\u{200B}b"), "ab");
    }

    #[test]
    fn words_pads_and_splits() {
        assert_eq!(words("Sign-in!"), " sign in ");
        assert_eq!(words("Del\u{200B}ete"), " delete ");
        assert_eq!(words(""), " ");
    }

    #[test]
    fn bounded_containment() {
        assert!(contains_bounded("open notepad please", "notepad"));
        assert!(!contains_bounded("open notepadplus", "notepad"));
        assert!(!contains_bounded("john smithson", "smith"));
        assert!(contains_bounded("hello, world!", "hello, world!"));
        assert!(!contains_bounded("abc", ""));
        assert!(contains_bounded("xnote note", "note"));
    }
}
