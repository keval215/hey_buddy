//! English text normalisation for TTS frontends (pure functions, no model, no deps).
//!
//! [`normalize`] turns written text into plain speakable words: numbers, years, dates, times,
//! currency, percentages, units and common abbreviations are spelled out, URLs and emails are
//! replaced by "link" / "email address", emoji and symbols are dropped, repeated punctuation is
//! collapsed. Anything not recognised passes through unchanged. It never panics.
//!
//! Choices worth knowing: numbers use Western scale words ("one hundred thousand"; Indian comma
//! grouping like `1,00,000` is accepted on input); numeric dates `a/b/yyyy` are read day-first
//! when ambiguous; "am"/"pm" are written "ay em"/"pee em" so a dictionary G2P reads them as letters.
//! The text is never logged or put in errors by this module.

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const SCALES: [&str; 6] = [
    "",
    "thousand",
    "million",
    "billion",
    "trillion",
    "quadrillion",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTH_ABBR: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// (written form incl. trailing dot, spoken form, keep a sentence stop after it)
const ABBR: &[(&str, &str, bool)] = &[
    ("mr.", "mister", false),
    ("mrs.", "missus", false),
    ("ms.", "miss", false),
    ("dr.", "doctor", false),
    ("prof.", "professor", false),
    ("jr.", "junior", false),
    ("sr.", "senior", false),
    ("vs.", "versus", false),
    ("e.g.", "for example", false),
    ("i.e.", "that is", false),
    ("etc.", "et cetera", true),
    ("approx.", "approximately", false),
    ("dept.", "department", false),
    ("fig.", "figure", false),
    ("inc.", "incorporated", false),
    ("ltd.", "limited", false),
    ("mt.", "mount", false),
    ("govt.", "government", false),
    ("a.k.a.", "also known as", false),
    ("a.m.", "ay em", false),
    ("p.m.", "pee em", false),
];
const ABBR_NO_DOT: &[(&str, &str)] = &[
    ("mr", "mister"),
    ("mrs", "missus"),
    ("dr", "doctor"),
    ("vs", "versus"),
    ("w/", "with"),
    ("w/o", "without"),
    ("b/c", "because"),
];

/// (key lowercase, singular, plural)
const UNITS: &[(&str, &str, &str)] = &[
    ("kg", "kilogram", "kilograms"),
    ("g", "gram", "grams"),
    ("mg", "milligram", "milligrams"),
    ("km", "kilometer", "kilometers"),
    ("m", "meter", "meters"),
    ("cm", "centimeter", "centimeters"),
    ("mm", "millimeter", "millimeters"),
    ("ft", "foot", "feet"),
    ("lb", "pound", "pounds"),
    ("lbs", "pound", "pounds"),
    ("oz", "ounce", "ounces"),
    ("l", "liter", "liters"),
    ("ml", "milliliter", "milliliters"),
    ("kb", "kilobyte", "kilobytes"),
    ("mb", "megabyte", "megabytes"),
    ("gb", "gigabyte", "gigabytes"),
    ("tb", "terabyte", "terabytes"),
    ("hz", "hertz", "hertz"),
    ("khz", "kilohertz", "kilohertz"),
    ("mhz", "megahertz", "megahertz"),
    ("ghz", "gigahertz", "gigahertz"),
    ("kw", "kilowatt", "kilowatts"),
    ("kwh", "kilowatt hour", "kilowatt hours"),
    ("mph", "mile per hour", "miles per hour"),
    ("kph", "kilometer per hour", "kilometers per hour"),
    ("kmph", "kilometer per hour", "kilometers per hour"),
    ("km/h", "kilometer per hour", "kilometers per hour"),
    ("kbps", "kilobit per second", "kilobits per second"),
    ("mbps", "megabit per second", "megabits per second"),
    ("ms", "millisecond", "milliseconds"),
    ("sec", "second", "seconds"),
    ("secs", "second", "seconds"),
    ("min", "minute", "minutes"),
    ("mins", "minute", "minutes"),
    ("hr", "hour", "hours"),
    ("hrs", "hour", "hours"),
    ("°c", "degree Celsius", "degrees Celsius"),
    ("°f", "degree Fahrenheit", "degrees Fahrenheit"),
    ("°", "degree", "degrees"),
];

/// (symbol, major singular, major plural, minor singular, minor plural)
const CURRENCIES: &[(char, &str, &str, &str, &str)] = &[
    ('$', "dollar", "dollars", "cent", "cents"),
    ('€', "euro", "euros", "cent", "cents"),
    ('£', "pound", "pounds", "penny", "pence"),
    ('₹', "rupee", "rupees", "paisa", "paise"),
];
const MAGNITUDES: &[&str] = &[
    "thousand", "million", "billion", "trillion", "lakh", "lakhs", "crore", "crores",
];
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "co", "in", "edu", "gov", "ai", "me", "uk", "de",
];
const YEAR_PREV: &[&str] = &[
    "in", "since", "from", "until", "till", "by", "year", "of", "before", "after", "during",
    "circa", "around", "mid", "early", "late",
];
const TRAIL: &str = ".,;:!?";

// ---------------------------------------------------------------- number words

/// Cardinal words for `n` (Western scale, no "and"): 105 -> "one hundred five".
pub fn cardinal(n: u64) -> String {
    if n == 0 {
        return "zero".into();
    }
    let mut groups = Vec::new();
    let mut rest = n;
    while rest > 0 {
        groups.push(rest % 1000);
        rest /= 1000;
    }
    let mut parts: Vec<String> = Vec::new();
    for (i, g) in groups.iter().enumerate().rev() {
        if *g == 0 {
            continue;
        }
        parts.push(below_1000(*g));
        if i > 0 {
            parts.push(SCALES.get(i).copied().unwrap_or("").to_string());
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" ")
}

fn below_1000(n: u64) -> String {
    let mut p: Vec<String> = Vec::new();
    let h = n / 100;
    let r = n % 100;
    if h > 0 {
        p.push(format!("{} hundred", ONES[h as usize]));
    }
    if r > 0 {
        if r < 20 {
            p.push(ONES[r as usize].into());
        } else {
            p.push(TENS[(r / 10) as usize].into());
            if !r.is_multiple_of(10) {
                p.push(ONES[(r % 10) as usize].into());
            }
        }
    }
    p.join(" ")
}

/// Ordinal words from cardinal words: "twenty one" -> "twenty first".
pub fn ordinal_words(card: &str) -> String {
    let (head, last) = match card.rfind(' ') {
        Some(i) => (&card[..=i], &card[i + 1..]),
        None => ("", card),
    };
    let o = match last {
        "one" => "first".to_string(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{o}")
}

fn digits_words(d: &str) -> String {
    d.chars()
        .filter_map(|c| c.to_digit(10))
        .map(|v| ONES[v as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words for a run of ASCII digits: cardinal when short and without a leading zero,
/// else digit by digit.
fn int_words(d: &str) -> String {
    if d.len() > 15 || (d.len() > 1 && d.starts_with('0')) {
        return digits_words(d);
    }
    d.parse::<u64>().map(cardinal).unwrap_or_default()
}

fn year_words(y: u32) -> String {
    let (hi, lo) = (u64::from(y / 100), u64::from(y % 100));
    if (2000..2010).contains(&y) {
        cardinal(u64::from(y))
    } else if lo == 0 {
        format!("{} hundred", cardinal(hi))
    } else if lo < 10 {
        format!("{} oh {}", cardinal(hi), ONES[lo as usize])
    } else {
        format!("{} {}", cardinal(hi), cardinal(lo))
    }
}

fn pluralize_last(words: &str) -> String {
    match words.strip_suffix('y') {
        Some(s) => format!("{s}ies"),
        None => format!("{words}s"),
    }
}

// ---------------------------------------------------------------- number parsing

#[derive(Debug, Clone)]
struct Num {
    sign: Option<char>,
    int: String,
    frac: Option<String>,
    rest: String,
}

impl Num {
    fn is_one(&self) -> bool {
        self.int == "1" && self.frac.is_none()
    }
    fn words(&self) -> String {
        let mut s = String::new();
        match self.sign {
            Some('-') => s.push_str("minus "),
            Some(_) => s.push_str("plus "),
            None => {}
        }
        s.push_str(&int_words(&self.int));
        if let Some(f) = &self.frac {
            s.push_str(" point ");
            s.push_str(&digits_words(f));
        }
        s
    }
}

fn valid_grouping(raw: &str) -> bool {
    let segs: Vec<&str> = raw.split(',').collect();
    if segs.len() < 2 || segs.iter().any(|s| s.is_empty()) {
        return false;
    }
    let first = segs[0].len();
    let last = segs[segs.len() - 1].len();
    if !(1..=3).contains(&first) || last != 3 {
        return false;
    }
    let mid = &segs[1..segs.len() - 1];
    mid.iter().all(|s| s.len() == 3) || (first <= 2 && mid.iter().all(|s| s.len() == 2))
}

/// Parse a leading number (`-1,234.5`, `.5`) and return it with the unparsed remainder.
fn split_number(s: &str) -> Option<Num> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut sign = None;
    if matches!(b.first(), Some('-' | '+')) {
        sign = Some(b[0]);
        i = 1;
    }
    let start = i;
    while i < b.len()
        && (b[i].is_ascii_digit()
            || (b[i] == ',' && i > start && b.get(i + 1).is_some_and(char::is_ascii_digit)))
    {
        i += 1;
    }
    let mut int: String = b[start..i].iter().collect();
    if int.is_empty() {
        if b.get(i) == Some(&'.') && b.get(i + 1).is_some_and(char::is_ascii_digit) {
            int = "0".into();
        } else {
            return None;
        }
    } else if int.contains(',') {
        if !valid_grouping(&int) {
            return None;
        }
        int.retain(|c| c != ',');
    }
    let mut frac = None;
    if b.get(i) == Some(&'.') && b.get(i + 1).is_some_and(char::is_ascii_digit) {
        let st = i + 1;
        i = st;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac = Some(b[st..i].iter().collect::<String>());
        // "1.2.3" (versions, section numbers) is not a number.
        if b.get(i) == Some(&'.') && b.get(i + 1).is_some_and(char::is_ascii_digit) {
            return None;
        }
    }
    Some(Num {
        sign,
        int,
        frac,
        rest: b[i..].iter().collect(),
    })
}

fn plain_int(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 9 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn ordinal_suffix(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "st" | "nd" | "rd" | "th")
}

/// A day of month: `5`, `5th` (no sign, no fraction).
fn day_of(core: &str) -> Option<u32> {
    let n = split_number(core)?;
    if n.sign.is_some() || n.frac.is_some() || !(n.rest.is_empty() || ordinal_suffix(&n.rest)) {
        return None;
    }
    plain_int(&n.int).filter(|d| (1..=31).contains(d))
}

fn year_of(core: &str) -> Option<u32> {
    if core.len() == 4 {
        plain_int(core).filter(|y| (1000..=2999).contains(y))
    } else {
        None
    }
}

fn month_of(core: &str) -> Option<usize> {
    let lower = core.to_ascii_lowercase();
    let cap = core.chars().next().is_some_and(char::is_uppercase);
    for (i, full) in MONTHS.iter().enumerate() {
        if lower == full.to_ascii_lowercase() && (cap || i != 4) {
            return Some(i);
        }
    }
    if cap {
        let l = lower.as_str();
        if l == "sept" {
            return Some(8);
        }
        if l != "may" {
            return MONTH_ABBR.iter().position(|a| *a == l);
        }
    }
    None
}

fn is_full_month(core: &str) -> bool {
    month_of(core).is_some_and(|m| MONTHS[m].eq_ignore_ascii_case(core))
}

fn unit_of(s: &str) -> Option<(&'static str, &'static str)> {
    let l = s.to_ascii_lowercase();
    UNITS
        .iter()
        .find(|(k, _, _)| *k == l)
        .map(|(_, a, b)| (*a, *b))
}

fn meridiem(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "am" | "a.m." | "a.m" => Some("ay em"),
        "pm" | "p.m." | "p.m" => Some("pee em"),
        _ => None,
    }
}

fn magnitude_suffix(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "k" => Some("thousand"),
        "m" | "mn" => Some("million"),
        "b" | "bn" => Some("billion"),
        _ => None,
    }
}

// ---------------------------------------------------------------- character cleaning

fn is_symbol_to_drop(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1F000..=0x1FFFF | 0x2190..=0x2BFF | 0x2100..=0x214F | 0xFE00..=0xFE0F
        | 0xE0000..=0xE007F | 0x20E3 | 0xA9 | 0xAE | 0x2022 | 0xB7 | 0xAC | 0xA6
        | 0xA4 | 0xA5 | 0xA2 | 0x20A0..=0x20AB | 0x20AD..=0x20B8 | 0x20BA..=0x20CF)
    // keep the arrows/maths we speak: none. (× and ° are handled before this.)
}

/// Remove control/bidi/zero-width characters and emoji/symbols, map brackets, dashes and
/// quotes to speakable punctuation.
fn clean_chars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\n' | '\r' | '\t' | '\u{A0}' | '\u{2028}' | '\u{2029}' | '\u{2002}'..='\u{200A}' => {
                out.push(' ')
            }
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{AD}' => {}
            c if c.is_control() => {}
            '(' | ')' | '[' | ']' | '{' | '}' | '\u{2014}' => out.push_str(" , "),
            '\u{2013}' => out.push('-'),
            '\u{2026}' => out.push('.'),
            '\u{2019}' | '\u{2018}' => out.push('\''),
            '"' | '\u{201C}' | '\u{201D}' | '\u{AB}' | '\u{BB}' | '`' => out.push(' '),
            '×' => out.push_str(" times "),
            c if is_symbol_to_drop(c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Collapse whitespace and runs of sentence punctuation; drop leading punctuation.
fn tidy(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let is_p = |c: char| ".,;:!?".contains(c);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            // Look past whitespace: a following punctuation run attaches to the previous word.
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && is_p(chars[j]) {
                i = j;
                continue;
            }
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            i = j;
            continue;
        }
        if is_p(c) {
            let mut j = i;
            let mut best = c;
            let rank = |c: char| match c {
                '?' => 5,
                '!' => 4,
                '.' => 3,
                ';' => 2,
                ':' => 1,
                _ => 0,
            };
            while j < chars.len() && (is_p(chars[j]) || chars[j].is_whitespace()) {
                if is_p(chars[j]) && rank(chars[j]) > rank(best) {
                    best = chars[j];
                }
                j += 1;
            }
            let glued_before = i > 0 && !chars[i - 1].is_whitespace();
            let glued_after = j < chars.len() && !chars[j - 1].is_whitespace();
            let single = j == i + 1;
            if single && glued_before && glued_after {
                out.push(c); // inside a token ("v1.2", "a:b")
            } else if !out.is_empty() {
                while out.ends_with(' ') {
                    out.pop();
                }
                if !out.is_empty() {
                    out.push(best);
                    if j < chars.len() {
                        out.push(' ');
                    }
                }
            }
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------- tokens

struct Tok {
    core: String,
    trail: String,
}

impl Tok {
    fn parse(w: &str) -> Tok {
        let cut = w.trim_end_matches(|c| TRAIL.contains(c)).len();
        let (c, t) = w.split_at(cut);
        let mut core = c.to_string();
        let mut trail = t.to_string();
        if trail.starts_with('.') {
            let cand = format!("{}.", core.to_ascii_lowercase());
            if ABBR.iter().any(|(k, _, _)| *k == cand) {
                core.push('.');
                trail.remove(0);
            }
        }
        Tok { core, trail }
    }
}

#[derive(Default)]
struct Ctx {
    prev_lower: String,
    date_ctx: bool,
    day_before_month: bool,
}

struct Out {
    text: String,
    consumed: usize,
    /// replaces the token's trail when set
    trail: Option<String>,
    date_ctx: bool,
    day_before_month: bool,
}

impl Out {
    fn new(text: impl Into<String>) -> Self {
        Out {
            text: text.into(),
            consumed: 0,
            trail: None,
            date_ctx: false,
            day_before_month: false,
        }
    }
}

fn looks_like_url(core: &str) -> bool {
    let l = core.to_ascii_lowercase();
    if l.contains("://") || l.starts_with("www.") {
        return true;
    }
    let host = l.split(['/', '?', '#']).next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels
            .last()
            .is_some_and(|t| TLDS.contains(t) && labels[0].chars().any(|c| c.is_ascii_alphabetic()))
}

fn looks_like_email(core: &str) -> bool {
    let mut it = core.split('@');
    match (it.next(), it.next(), it.next()) {
        (Some(l), Some(d), None) => {
            !l.is_empty()
                && !l.starts_with(['.', '+'])
                && d.contains('.')
                && !d.starts_with('.')
                && !d.ends_with('.')
                && d.rsplit('.')
                    .next()
                    .is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
        }
        _ => false,
    }
}

fn currency_prefix(core: &str) -> Option<(usize, &str)> {
    let first = core.chars().next()?;
    if let Some(i) = CURRENCIES.iter().position(|c| c.0 == first) {
        return Some((i, &core[first.len_utf8()..]));
    }
    let l = core.to_ascii_lowercase();
    for p in ["rs.", "rs", "inr"] {
        if l.starts_with(p) && core.is_char_boundary(p.len()) {
            let rest = &core[p.len()..];
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                return Some((3, rest));
            }
        }
    }
    None
}

fn currency_words(ci: usize, n: &Num, mag: Option<&str>) -> String {
    let (_, maj1, majn, min1, minn) = CURRENCIES[ci];
    let sign = if n.sign == Some('-') { "minus " } else { "" };
    if let Some(m) = mag {
        return format!("{sign}{} {m} {majn}", n.words_unsigned());
    }
    match &n.frac {
        None => {
            let unit = if n.is_one() { maj1 } else { majn };
            format!("{sign}{} {unit}", int_words(&n.int))
        }
        Some(f) if f.len() <= 2 => {
            let cents: u32 = format!("{f:0<2}").parse().unwrap_or(0);
            let major_zero = n.int.chars().all(|c| c == '0');
            let mut parts = Vec::new();
            if !major_zero || cents == 0 {
                let unit = if n.int == "1" { maj1 } else { majn };
                parts.push(format!("{} {unit}", int_words(&n.int)));
            }
            if cents > 0 {
                let unit = if cents == 1 { min1 } else { minn };
                parts.push(format!("{} {unit}", cardinal(u64::from(cents))));
            }
            format!("{sign}{}", parts.join(" "))
        }
        Some(_) => format!("{sign}{} {majn}", n.words_unsigned()),
    }
}

impl Num {
    fn words_unsigned(&self) -> String {
        let mut s = int_words(&self.int);
        if let Some(f) = &self.frac {
            s.push_str(" point ");
            s.push_str(&digits_words(f));
        }
        s
    }
}

fn time_words(core: &str, next: Option<&Tok>) -> Option<Out> {
    let (body, glued) = {
        let l = core.to_ascii_lowercase();
        let mut found = None;
        for suffix in ["am", "pm", "a.m.", "p.m.", "a.m", "p.m"] {
            if let Some(b) = l.strip_suffix(suffix) {
                if !b.is_empty() {
                    found = Some((core[..b.len()].to_string(), meridiem(suffix)));
                    break;
                }
            }
        }
        found.unwrap_or((core.to_string(), None))
    };
    let (h, m) = body.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let hour = plain_int(h)?;
    let min = plain_int(m)?;
    if hour > 24 || min > 59 {
        return None;
    }
    let mut consumed = 0;
    let mut mer = glued;
    let mut trail = None;
    if mer.is_none() {
        if let Some(n) = next {
            if let Some(w) = meridiem(&n.core) {
                mer = Some(w);
                consumed = 1;
                trail = Some(n.trail.clone());
            }
        }
    }
    let mut s = cardinal(u64::from(hour));
    if min == 0 {
        if mer.is_none() {
            s.push_str(if hour >= 13 || h.starts_with('0') {
                " hundred"
            } else {
                " o'clock"
            });
        }
    } else if min < 10 {
        s.push_str(&format!(" oh {}", ONES[min as usize]));
    } else {
        s.push_str(&format!(" {}", cardinal(u64::from(min))));
    }
    if let Some(w) = mer {
        s.push(' ');
        s.push_str(w);
    }
    let mut o = Out::new(s);
    o.consumed = consumed;
    o.trail = trail;
    Some(o)
}

fn date_words(core: &str) -> Option<Out> {
    let sep = ['-', '/', '.']
        .into_iter()
        .find(|s| core.matches(*s).count() == 2)?;
    let parts: Vec<&str> = core.split(sep).collect();
    if parts.len() != 3 || parts.iter().any(|p| plain_int(p).is_none()) {
        return None;
    }
    let (a, b, c) = (
        plain_int(parts[0])?,
        plain_int(parts[1])?,
        plain_int(parts[2])?,
    );
    let (year, month, day) = if parts[0].len() == 4 {
        (a, b, c) // y-m-d
    } else if parts[2].len() == 4 || parts[2].len() == 2 {
        let year = if parts[2].len() == 2 {
            if c < 50 {
                2000 + c
            } else {
                1900 + c
            }
        } else {
            c
        };
        if parts[0].len() > 2 || parts[1].len() > 2 {
            return None;
        }
        if sep == '.' && parts[2].len() == 2 {
            return None; // looks like a version number
        }
        // day-first unless the second field cannot be a month... or the first cannot be a day
        if b > 12 && a <= 12 {
            (year, a, b)
        } else {
            (year, b, a)
        }
    } else {
        return None;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year == 0 || year > 9999 {
        return None;
    }
    let yw = if (1000..=2999).contains(&year) {
        year_words(year)
    } else {
        cardinal(u64::from(year))
    };
    let mut o = Out::new(format!(
        "{} {} {}",
        MONTHS[month as usize - 1],
        ordinal_words(&cardinal(u64::from(day))),
        yw
    ));
    o.date_ctx = false;
    Some(o)
}

fn range_or_digits(core: &str) -> Option<Out> {
    let parts: Vec<&str> = core.split('-').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty() || plain_int(p).is_none()) {
        return None;
    }
    if parts.len() == 2
        && parts.iter().all(|p| p.len() <= 4 && !p.starts_with('0'))
        && plain_int(parts[0])? < plain_int(parts[1])?
    {
        return Some(Out::new(format!(
            "{} to {}",
            int_words(parts[0]),
            int_words(parts[1])
        )));
    }
    Some(Out::new(
        parts
            .iter()
            .map(|p| digits_words(p))
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

fn handle_number(core: &str, next: Option<&Tok>, ctx: &Ctx) -> Option<Out> {
    let mut core = core;
    let mut about = false;
    if let Some(r) = core.strip_prefix('~') {
        if r.starts_with(|c: char| c.is_ascii_digit()) {
            core = r;
            about = true;
        }
    }
    let n = split_number(core)?;
    let pre = if about { "about " } else { "" };
    let rest = n.rest.to_ascii_lowercase();
    let simple = n.sign.is_none() && n.frac.is_none();
    let mut o = match rest.as_str() {
        "" => {
            let mut o = Out::new(String::new());
            let year_ctx = ctx.date_ctx || YEAR_PREV.contains(&ctx.prev_lower.as_str());
            if simple && year_ctx && n.int.len() == 4 && year_of(&n.int).is_some() {
                o.text = year_words(plain_int(&n.int)?);
                return Some(o);
            }
            if let (true, Some(d), Some(nx)) = (simple, day_of(core), next) {
                if month_of(&nx.core).is_some() {
                    o.text = format!("{} of", ordinal_words(&cardinal(u64::from(d))));
                    o.day_before_month = true;
                    return Some(o);
                }
            }
            o.text = n.words();
            if let Some(nx) = next {
                let l = nx.core.to_ascii_lowercase();
                if nx.core == "%" {
                    o.text.push_str(" percent");
                    o.consumed = 1;
                    o.trail = Some(nx.trail.clone());
                } else if let (Some(w), true, Some(h)) =
                    (meridiem(&nx.core), simple, plain_int(&n.int))
                {
                    if (1..=12).contains(&h) {
                        o.text.push_str(&format!(" {w}"));
                        o.consumed = 1;
                        o.trail = Some(nx.trail.clone());
                    }
                } else if let Some((s1, sn)) = unit_of(&l) {
                    o.text
                        .push_str(&format!(" {}", if n.is_one() { s1 } else { sn }));
                    o.consumed = 1;
                    o.trail = Some(nx.trail.clone());
                }
            }
            o
        }
        "%" => Out::new(format!("{} percent", n.words())),
        r if ordinal_suffix(r) && simple => {
            Out::new(ordinal_words(&cardinal(plain_int(&n.int).map(u64::from)?)))
        }
        "s" | "'s" if simple && n.int.len() == 4 && year_of(&n.int).is_some() => {
            Out::new(pluralize_last(&year_words(plain_int(&n.int)?)))
        }
        "am" | "pm" | "a.m." | "p.m." if simple => {
            let h = plain_int(&n.int)?;
            if !(1..=12).contains(&h) {
                return None;
            }
            Out::new(format!("{} {}", n.words(), meridiem(&rest)?))
        }
        r => {
            if let Some(m) = magnitude_suffix(r).filter(|_| r.len() > 1 || r == "k") {
                Out::new(format!("{} {m}", n.words()))
            } else if let Some((s1, sn)) = unit_of(r) {
                Out::new(format!(
                    "{} {}",
                    n.words(),
                    if n.is_one() { s1 } else { sn }
                ))
            } else {
                return None;
            }
        }
    };
    if about {
        o.text = format!("{pre}{}", o.text);
    }
    Some(o)
}

fn handle_currency(core: &str, next: Option<&Tok>) -> Option<Out> {
    let (ci, rest) = currency_prefix(core)?;
    let n = split_number(rest)?;
    let mut o;
    if n.rest.is_empty() {
        if let Some(nx) =
            next.filter(|nx| MAGNITUDES.contains(&nx.core.to_ascii_lowercase().as_str()))
        {
            o = Out::new(currency_words(ci, &n, Some(&nx.core.to_ascii_lowercase())));
            o.consumed = 1;
            o.trail = Some(nx.trail.clone());
        } else {
            o = Out::new(currency_words(ci, &n, None));
        }
    } else {
        let m = magnitude_suffix(&n.rest)?;
        o = Out::new(currency_words(ci, &n, Some(m)));
    }
    Some(o)
}

fn passthrough(core: &str) -> String {
    let has_digit = core.chars().any(|c| c.is_ascii_digit());
    core.chars()
        .filter_map(|c| match c {
            '*' | '|' | '^' | '\\' | '#' | '<' | '>' | '~' | '=' | '{' | '}' => None,
            '_' => Some(' '),
            '/' if !has_digit => Some(' '),
            c => Some(c),
        })
        .collect()
}

fn handle(toks: &[Tok], i: usize, ctx: &Ctx) -> Out {
    let core = toks[i].core.as_str();
    let next = toks.get(i + 1);
    let lower = core.to_ascii_lowercase();
    if let Some((_, spoken, stop)) = ABBR.iter().find(|(k, _, _)| *k == lower) {
        let mut o = Out::new(*spoken);
        if *stop && toks[i].trail.is_empty() {
            o.trail = Some(".".into());
        }
        return o;
    }
    if let Some((_, spoken)) = ABBR_NO_DOT.iter().find(|(k, _)| *k == lower) {
        return Out::new(*spoken);
    }
    match core {
        "&" => return Out::new("and"),
        "+" => return Out::new("plus"),
        "=" => return Out::new("equals"),
        "@" => return Out::new("at"),
        "%" => return Out::new("percent"),
        "<" => return Out::new("less than"),
        ">" => return Out::new("greater than"),
        "-" | "--" | "---" => {
            let mut o = Out::new("");
            o.trail = Some(",".into());
            return o;
        }
        _ => {}
    }
    if looks_like_email(core) {
        return Out::new("email address");
    }
    if looks_like_url(core) {
        return Out::new("link");
    }
    if (lower == "rs" || lower == "inr") && (toks[i].trail.is_empty() || toks[i].trail == ".") {
        if let Some(nx) = next {
            if let Some(n) = split_number(&nx.core).filter(|n| n.rest.is_empty()) {
                let mag = toks
                    .get(i + 2)
                    .filter(|t| MAGNITUDES.contains(&t.core.to_ascii_lowercase().as_str()));
                let mut o = Out::new(currency_words(
                    3,
                    &n,
                    mag.map(|t| t.core.to_ascii_lowercase()).as_deref(),
                ));
                o.consumed = 1 + usize::from(mag.is_some());
                o.trail = Some(toks[i + o.consumed].trail.clone());
                return o;
            }
        }
    }
    if let Some(o) = handle_currency(core, next) {
        return o;
    }
    if let Some(o) = date_words(core) {
        return o;
    }
    if let Some(o) = time_words(core, next) {
        return o;
    }
    // Month names: "March 5, 2024", "Mar 5th", and (after "5th") "March 2024".
    if let Some(m) = month_of(core) {
        let nx = next;
        let day = nx.and_then(|t| day_of(&t.core));
        if let (Some(d), Some(t)) = (day, nx) {
            let mut o = Out::new(format!(
                "{} {}",
                MONTHS[m],
                ordinal_words(&cardinal(u64::from(d)))
            ));
            o.consumed = 1;
            o.trail = Some(t.trail.clone());
            o.date_ctx = true;
            return o;
        }
        if ctx.day_before_month
            || nx.is_some_and(|t| year_of(&t.core).is_some() && is_full_month(core))
        {
            let mut o = Out::new(MONTHS[m]);
            o.date_ctx = true;
            if !is_full_month(core) {
                o.trail = Some(toks[i].trail.trim_start_matches('.').to_string());
            }
            return o;
        }
    }
    if core.contains('-') {
        if let Some(o) = range_or_digits(core) {
            return o;
        }
    }
    if let Some(o) = handle_number(core, next, ctx) {
        return o;
    }
    // "1/2" -> "one over two"
    if let Some((a, b)) = core.split_once('/') {
        if let (Some(x), Some(y)) = (plain_int(a), plain_int(b)) {
            return Out::new(format!(
                "{} over {}",
                cardinal(u64::from(x)),
                cardinal(u64::from(y))
            ));
        }
    }
    // Comma lists that are not thousands grouping: "1,2,3".
    if core.contains(',') && core.chars().any(|c| c.is_ascii_digit()) {
        let parts: Vec<String> = core
            .split(',')
            .filter(|p| !p.is_empty())
            .map(normalize)
            .collect();
        return Out::new(parts.join(", "));
    }
    Out::new(passthrough(core))
}

/// Normalise English text for speech. Total: any input gives a string, never a panic.
pub fn normalize(input: &str) -> String {
    let cleaned = clean_chars(input);
    let toks: Vec<Tok> = cleaned.split_whitespace().map(Tok::parse).collect();
    let mut out = String::with_capacity(cleaned.len() + 16);
    let mut ctx = Ctx::default();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let cur = Ctx {
            prev_lower: std::mem::take(&mut ctx.prev_lower),
            date_ctx: std::mem::take(&mut ctx.date_ctx),
            day_before_month: std::mem::take(&mut ctx.day_before_month),
        };
        let o = handle(&toks, i, &cur);
        let trail = o.trail.as_deref().unwrap_or(&t.trail);
        out.push(' ');
        out.push_str(&o.text);
        out.push_str(trail);
        ctx.prev_lower = toks[(i + o.consumed).min(toks.len() - 1)]
            .core
            .to_ascii_lowercase();
        ctx.date_ctx = o.date_ctx;
        ctx.day_before_month = o.day_before_month;
        i += 1 + o.consumed;
    }
    tidy(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinals_and_ordinals() {
        let c = [
            (0, "zero"),
            (7, "seven"),
            (13, "thirteen"),
            (21, "twenty one"),
            (100, "one hundred"),
            (105, "one hundred five"),
            (1000, "one thousand"),
            (100_000, "one hundred thousand"),
            (
                1_234_567,
                "one million two hundred thirty four thousand five hundred sixty seven",
            ),
        ];
        for (n, w) in c {
            assert_eq!(cardinal(n), w);
        }
        assert_eq!(ordinal_words("twenty one"), "twenty first");
        assert_eq!(ordinal_words("twelve"), "twelfth");
        assert_eq!(ordinal_words("thirty"), "thirtieth");
        assert_eq!(ordinal_words("four"), "fourth");
        assert_eq!(ordinal_words("eight"), "eighth");
    }

    #[test]
    fn fixtures() {
        let cases: &[(&str, &str)] = &[
            // plain text and unknown tokens pass through
            ("Hello there, how are you?", "Hello there, how are you?"),
            ("Привет мир", "Привет мир"),
            ("v1.2.3 is out", "v1.2.3 is out"),
            ("B2B sales", "B2B sales"),
            // numbers
            ("I have 3 cats", "I have three cats"),
            (
                "There are 1,234 users",
                "There are one thousand two hundred thirty four users",
            ),
            ("Population 1,00,000", "Population one hundred thousand"),
            (
                "12,34,567 people",
                "one million two hundred thirty four thousand five hundred sixty seven people",
            ),
            ("It is -5 outside", "It is minus five outside"),
            ("Pi is 3.14", "Pi is three point one four"),
            ("About .5 left", "About zero point five left"),
            ("Room 007", "Room zero zero seven"),
            ("Count 1,2,3", "Count one, two, three"),
            ("Pages 5-10", "Pages five to ten"),
            (
                "Call 555-123-4567",
                "Call five five five, one two three, four five six seven",
            ),
            ("~20 items", "about twenty items"),
            ("1/2 cup", "one over two cup"),
            // ordinals
            (
                "the 1st, 2nd and 23rd",
                "the first, second and twenty third",
            ),
            ("Finish 3rd", "Finish third"),
            // years
            ("In 2024 we shipped", "In twenty twenty four we shipped"),
            ("since 1999.", "since nineteen ninety nine."),
            ("in 2005", "in two thousand five"),
            ("in 1905", "in nineteen oh five"),
            ("in 1900", "in nineteen hundred"),
            ("the 1990s were fun", "the nineteen nineties were fun"),
            (
                "I paid 2024 for it",
                "I paid two thousand twenty four for it",
            ),
            // dates
            ("Due 2024-03-05", "Due March fifth twenty twenty four"),
            ("On 5/3/2024", "On March fifth twenty twenty four"),
            (
                "On 25/12/2023",
                "On December twenty fifth twenty twenty three",
            ),
            (
                "On 12/25/2023",
                "On December twenty fifth twenty twenty three",
            ),
            (
                "On March 5, 2024 we met",
                "On March fifth, twenty twenty four we met",
            ),
            ("Mar 5th", "March fifth"),
            ("Mar. 5 works", "March fifth works"),
            ("on 5 March 2024", "on fifth of March twenty twenty four"),
            ("the 21st of May", "the twenty first of May"),
            ("It may 5 happen", "It may five happen"),
            // times
            ("At 3:45 pm sharp", "At three forty five pee em sharp"),
            ("At 3:45pm", "At three forty five pee em"),
            ("At 3:05 PM.", "At three oh five pee em."),
            ("At 10:00 a.m. today", "At ten ay em today"),
            ("Train 15:30", "Train fifteen thirty"),
            ("Train 15:00", "Train fifteen hundred"),
            ("Meet at 3:00", "Meet at three o'clock"),
            ("Wake at 7 am", "Wake at seven ay em"),
            ("Open 9am", "Open nine ay em"),
            ("ratio 3:99", "ratio 3:99"),
            // currency
            ("It costs $5", "It costs five dollars"),
            ("It costs $1", "It costs one dollar"),
            ("It costs $5.50", "It costs five dollars fifty cents"),
            ("Only $0.99", "Only ninety nine cents"),
            ("Only $3.05", "Only three dollars five cents"),
            ("₹500 only", "five hundred rupees only"),
            ("₹1,00,000 total", "one hundred thousand rupees total"),
            ("Rs. 250 per kg", "two hundred fifty rupees per kg"),
            ("Rs.99", "ninety nine rupees"),
            ("Rs 1,500", "one thousand five hundred rupees"),
            (
                "€20 and £7.25",
                "twenty euros and seven pounds twenty five pence",
            ),
            ("$5 million deal", "five million dollars deal"),
            ("₹2 crore", "two crore rupees"),
            ("$5k", "five thousand dollars"),
            ("$-3", "minus three dollars"),
            // percentages
            ("50% off", "fifty percent off"),
            ("up 12.5 %", "up twelve point five percent"),
            ("100 % sure", "one hundred percent sure"),
            // units
            ("5kg of rice", "five kilograms of rice"),
            ("1 kg", "one kilogram"),
            ("12 km.", "twelve kilometers."),
            ("a 500 MB file", "a five hundred megabytes file"),
            ("25°C today", "twenty five degrees Celsius today"),
            ("Run 5 km/h", "Run five kilometers per hour"),
            ("2.4GHz", "two point four gigahertz"),
            // abbreviations
            ("Mr. Smith and Dr. Jones", "mister Smith and doctor Jones"),
            (
                "Use fruit, e.g. apples, i.e. fresh.",
                "Use fruit, for example apples, that is fresh.",
            ),
            (
                "apples, pears, etc. are good",
                "apples, pears, et cetera. are good",
            ),
            ("Cats vs. dogs", "Cats versus dogs"),
            ("Mrs Brown", "missus Brown"),
            // urls and emails
            ("See https://example.com/a?b=1 now", "See link now"),
            ("Visit www.example.org.", "Visit link."),
            ("Try example.com/docs", "Try link"),
            ("Mail me@example.com please", "Mail email address please"),
            ("file.txt stays", "file.txt stays"),
            ("a@b is not mail", "a@b is not mail"),
            // emoji and symbols
            ("Great job 🎉🎉 team 👍", "Great job team"),
            ("Done ✅", "Done"),
            ("Brand™ and © 2020", "Brand and two thousand twenty"),
            ("Tom & Jerry", "Tom and Jerry"),
            ("2 × 3", "two times three"),
            ("a * b", "a b"),
            ("snake_case word", "snake case word"),
            ("and/or", "and or"),
            // punctuation
            ("Wow!!!", "Wow!"),
            ("What?!", "What?"),
            ("Wait... what", "Wait. what"),
            ("Hi   there ,  you", "Hi there, you"),
            ("Well -- okay", "Well, okay"),
            ("Well — okay", "Well, okay"),
            ("“Quoted” text", "Quoted text"),
            // parentheses and brackets
            ("Call me (maybe) now", "Call me, maybe, now"),
            ("See [1] below", "See, one, below"),
            ("Done (really).", "Done, really."),
            // control and bidi characters
            ("he\u{202E}llo\u{200B} wor\u{0}ld\u{FEFF}", "hello world"),
            ("tab\tsep\nnew", "tab sep new"),
            ("", ""),
            ("   ", ""),
            ("...", ""),
        ];
        let mut bad = Vec::new();
        for (input, want) in cases {
            let got = normalize(input);
            if got != *want {
                bad.push(format!("{input:?}\n   got  {got:?}\n   want {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn huge_numbers_do_not_panic() {
        let long = "9".repeat(200);
        assert!(!normalize(&long).is_empty());
        assert!(!normalize(&format!("${long}.{long}")).is_empty());
        assert!(!normalize(&format!("{long}%")).is_empty());
        assert!(!normalize("18446744073709551616 99999999999999999999").is_empty());
        assert_eq!(normalize("999999999999999"), cardinal(999_999_999_999_999));
    }

    const TRICKY: &str = "İẞǅǈǋŉΐΰǰßﬁﬂﬃſKÅ";

    #[test]
    fn case_folding_regressions_do_not_panic() {
        let tricky: Vec<char> = TRICKY.chars().collect();
        let suffixes = [
            "am", "pm", "a.m.", "p.m.", "rs", "rs.", "inr", "km", "kg", "mb", "st", "nd", "rd",
            "th",
        ];
        for t in tricky
            .iter()
            .map(|c| c.to_string())
            .chain(["İİİİ", "ẞẞ", "ǅǅ", "ŉŉ", "ΐΐ"].map(String::from))
        {
            let _ = normalize(&t);
            for suf in suffixes {
                for s in [
                    format!("{t}{suf}"),
                    format!("{suf}{t}"),
                    format!("{t}{t}{suf}"),
                    format!("3:45{t}{suf}"),
                    format!("3:45{suf}{t}"),
                    format!("3:45 {t}{suf}"),
                    format!("5{t}{suf}"),
                    format!("5{suf}{t}"),
                    format!("{suf}5{t}"),
                    format!("{t}{suf}5"),
                    format!("$5{t}{suf}"),
                    format!("{t}{suf}. 250"),
                    format!("{suf}{t}250"),
                    format!("{t} {suf}"),
                    format!("{t}{suf}{t}"),
                ] {
                    let _ = normalize(&s);
                }
            }
        }
        for s in ["İİİİam", "3:45İİİİpm", "İ", "ẞ", "ǅ", "ǈ", "ŉ", "ΐ"] {
            let _ = normalize(s);
        }
        // The time still reads correctly next to such characters.
        assert_eq!(normalize("3:45pm"), normalize("3:45PM"));
    }

    /// Every suffix, prefix, unit, currency and abbreviation the normaliser knows.
    fn known_words() -> Vec<String> {
        let mut w: Vec<String> = Vec::new();
        w.extend(ABBR.iter().map(|(k, _, _)| k.to_string()));
        w.extend(ABBR_NO_DOT.iter().map(|(k, _)| k.to_string()));
        w.extend(UNITS.iter().map(|(k, _, _)| k.to_string()));
        w.extend(MONTHS.iter().map(|m| m.to_string()));
        w.extend(MONTH_ABBR.iter().map(|m| m.to_string()));
        w.extend(MAGNITUDES.iter().map(|m| m.to_string()));
        w.extend(TLDS.iter().map(|m| format!("a.{m}")));
        w.extend(YEAR_PREV.iter().map(|m| m.to_string()));
        w.extend(CURRENCIES.iter().map(|c| c.0.to_string()));
        for s in [
            "am", "pm", "a.m.", "p.m.", "a.m", "p.m", "rs", "rs.", "inr", "st", "nd", "rd", "th",
            "k", "m", "b", "mn", "bn", "x", "%", "~", "@", "://", "www.", "e-mail",
        ] {
            w.push(s.to_string());
        }
        w
    }

    #[test]
    fn random_unicode_never_panics() {
        // Small deterministic LCG; no external crates.
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32
        };
        let alphabet: Vec<char> = "0123456789.,:;/-+%$€£₹ @#~*()[]\"'!?&_ \tampmkgst"
            .chars()
            .collect();
        for _ in 0..3000 {
            let len = (next() % 40) as usize;
            let s: String = (0..len)
                .map(|_| match next() % 4 {
                    0 => char::from_u32(next() % 0x11_0000).unwrap_or('?'),
                    1 => char::from_u32(0x1F300 + next() % 0x400).unwrap_or('?'),
                    _ => alphabet[(next() as usize) % alphabet.len()],
                })
                .collect();
            let _ = normalize(&s);
        }

        // Adversarial: case-folding / length-changing chars + digits + every known word.
        let tricky: Vec<char> = TRICKY.chars().collect();
        let words = known_words();
        let glue = ["", "", " ", ":", ".", ",", "-", "/", "'"];
        for _ in 0..6000 {
            let pieces = 1 + (next() % 6) as usize;
            let mut s = String::new();
            for _ in 0..pieces {
                match next() % 4 {
                    0 => s.push(tricky[(next() as usize) % tricky.len()]),
                    1 => {
                        for _ in 0..1 + next() % 4 {
                            s.push(char::from(b'0' + (next() % 10) as u8));
                        }
                    }
                    _ => s.push_str(&words[(next() as usize) % words.len()]),
                }
                s.push_str(glue[(next() as usize) % glue.len()]);
            }
            let _ = normalize(&s);
        }
    }

    #[test]
    fn huge_inputs_finish_in_bounded_time() {
        use std::time::{Duration, Instant};
        let cases: Vec<(&str, String)> = vec![
            ("1 MB digits", "7".repeat(1_000_000)),
            ("100k punctuation", "!?.,;:".repeat(100_000 / 6 + 1)),
            ("100k dots", ".".repeat(100_000)),
            ("300k '1,'", "1,".repeat(300_000)),
        ];
        for (name, input) in cases {
            let t = Instant::now();
            let _ = normalize(&input);
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "{name} took {:?}",
                t.elapsed()
            );
        }
    }
}
