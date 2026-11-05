//! Title / URL leak hardening (S2-5), pure and dependency-free.
//!
//! Two jobs, both of which can only REMOVE information:
//! 1. [`scrub_links`]: replace URL-like substrings with `[link]`, opaque token-like strings with
//!    `[token]` and e-mail addresses with `[email]` in every label / value that is emitted.
//! 2. Label lexicons that decide whether a whole subtree is browser chrome that must never be
//!    emitted (address bar, tab strip, bookmarks bar, downloads shelf).
//!
//! Both fail toward redaction; the false positives are listed in `docs/title-url-audit.md`.

const SCHEMELESS: &[&str] = &[
    "mailto",
    "file",
    "javascript",
    "vbscript",
    "data",
    "tel",
    "sms",
];

/// Top-level domains for which a bare `name.tld` (no path) is treated as a host. Deliberately
/// excludes TLDs that double as common file extensions or words (py, sh, rs, md, js, pl, cc, so).
const KNOWN_TLDS: &[&str] = &[
    "com",
    "net",
    "org",
    "edu",
    "gov",
    "mil",
    "int",
    "io",
    "co",
    "uk",
    "de",
    "fr",
    "ru",
    "cn",
    "jp",
    "info",
    "biz",
    "dev",
    "app",
    "xyz",
    "me",
    "ai",
    "us",
    "ca",
    "au",
    "in",
    "br",
    "it",
    "es",
    "nl",
    "se",
    "no",
    "ch",
    "eu",
    "tv",
    "ly",
    "gg",
    "to",
    "at",
    "be",
    "dk",
    "fi",
    "pt",
    "example",
    "test",
    "local",
    "internal",
    "invalid",
    "localhost",
    "onion",
    "lan",
    "home",
    "corp",
    "online",
    "site",
    "store",
    "tech",
    "cloud",
    "page",
    "link",
    "click",
    "top",
    "club",
    "live",
    "shop",
    "blog",
    "pro",
    "name",
    "mobi",
];

fn is_tok(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.')
}

fn starts_ci(c: &[char], i: usize, pat: &str) -> bool {
    let mut k = i;
    for p in pat.chars() {
        match c.get(k) {
            Some(x) if x.to_ascii_lowercase() == p => k += 1,
            _ => return false,
        }
    }
    true
}

/// End (exclusive) of a URL-ish run starting at `start`: stops at whitespace, `‹ › < > "` and an
/// unmatched closing bracket; trailing sentence punctuation is not part of the URL.
fn url_end(c: &[char], start: usize) -> usize {
    let (mut paren, mut square, mut curly) = (0i32, 0i32, 0i32);
    let mut j = start;
    while j < c.len() {
        let x = c[j];
        if x.is_whitespace() || matches!(x, '‹' | '›' | '<' | '>' | '"') {
            break;
        }
        match x {
            '(' => paren += 1,
            '[' => square += 1,
            '{' => curly += 1,
            ')' => {
                if paren == 0 {
                    break;
                }
                paren -= 1;
            }
            ']' => {
                if square == 0 {
                    break;
                }
                square -= 1;
            }
            '}' => {
                if curly == 0 {
                    break;
                }
                curly -= 1;
            }
            _ => {}
        }
        j += 1;
    }
    while j > start
        && matches!(
            c[j - 1],
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '…' | '*'
        )
    {
        j -= 1;
    }
    j
}

fn digits_end(c: &[char], i: usize) -> usize {
    let mut j = i;
    while j < c.len() && c[j].is_ascii_digit() {
        j += 1;
    }
    j
}

/// `host[:port][/path?query#frag]` tail: after the host ended at `h`.
fn tail(c: &[char], h: usize) -> usize {
    let mut e = h;
    if c.get(e) == Some(&':') && c.get(e + 1).is_some_and(|x| x.is_ascii_digit()) {
        e = digits_end(c, e + 1);
    }
    if matches!(c.get(e), Some('/') | Some('?') | Some('#')) {
        e = url_end(c, e).max(e);
    }
    e
}

fn match_scheme(c: &[char], i: usize) -> Option<usize> {
    if !c.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    let mut j = i;
    while j < c.len() && (c[j].is_ascii_alphanumeric() || matches!(c[j], '+' | '-' | '.')) {
        j += 1;
    }
    if c.get(j) == Some(&':') && c.get(j + 1) == Some(&'/') && c.get(j + 2) == Some(&'/') {
        return Some(url_end(c, i).max(j + 3));
    }
    None
}

fn match_special(c: &[char], i: usize) -> Option<usize> {
    for name in SCHEMELESS {
        if starts_ci(c, i, name) {
            let k = i + name.len();
            if c.get(k) == Some(&':') && c.get(k + 1).is_some_and(|x| !x.is_whitespace()) {
                return Some(url_end(c, i).max(k + 2));
            }
        }
    }
    None
}

fn match_www(c: &[char], i: usize) -> Option<usize> {
    if starts_ci(c, i, "www.") && c.get(i + 4).is_some_and(|x| x.is_alphanumeric()) {
        return Some(url_end(c, i).max(i + 5));
    }
    None
}

/// `?a=b`, `&a=b`, `#a=b`: a query string or fragment parameter on its own.
fn match_query(c: &[char], i: usize) -> Option<usize> {
    if !matches!(c[i], '?' | '&' | '#') {
        return None;
    }
    let mut j = i + 1;
    while j < c.len() && (c[j].is_ascii_alphanumeric() || matches!(c[j], '_' | '-' | '.')) {
        j += 1;
    }
    if j > i + 1 && c.get(j) == Some(&'=') {
        return Some(url_end(c, i).max(j + 1));
    }
    None
}

fn match_email(c: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    while j < c.len() && (c[j].is_alphanumeric() || matches!(c[j], '.' | '_' | '%' | '+' | '-')) {
        j += 1;
    }
    if j == i || c.get(j) != Some(&'@') {
        return None;
    }
    let ds = j + 1;
    let mut k = ds;
    while k < c.len() && (c[k].is_alphanumeric() || matches!(c[k], '.' | '-')) {
        k += 1;
    }
    while k > ds && matches!(c[k - 1], '.' | '-') {
        k -= 1;
    }
    let dom = &c[ds..k];
    let dot = dom.iter().rposition(|&x| x == '.')?;
    let tld = &dom[dot + 1..];
    (tld.len() >= 2 && tld.iter().all(|x| x.is_alphabetic())).then_some(k)
}

fn match_localhost(c: &[char], i: usize) -> Option<usize> {
    if !starts_ci(c, i, "localhost") {
        return None;
    }
    let k = i + 9;
    match c.get(k) {
        Some(':') if c.get(k + 1).is_some_and(|x| x.is_ascii_digit()) => Some(tail(c, k)),
        Some('/') => Some(tail(c, k)),
        _ => None,
    }
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn match_ipv6(c: &[char], i: usize) -> Option<usize> {
    if c[i] == '[' {
        let mut j = i + 1;
        while j < c.len() && (is_hex(c[j]) || matches!(c[j], ':' | '.')) {
            j += 1;
        }
        let body = &c[i + 1..j];
        if c.get(j) == Some(&']')
            && body.iter().filter(|&&x| x == ':').count() >= 2
            && body.iter().any(|&x| is_hex(x))
        {
            return Some(tail(c, j + 1));
        }
        return None;
    }
    let mut j = i;
    while j < c.len() && (is_hex(c[j]) || c[j] == ':') {
        j += 1;
    }
    let run = &c[i..j];
    let colons = run.iter().filter(|&&x| x == ':').count();
    let double = run.windows(2).any(|w| w == [':', ':']);
    let groups_ok = run
        .split(|&x| x == ':')
        .all(|g| g.len() <= 4 && g.iter().all(|&x| is_hex(x)));
    if colons >= 2
        && (double || colons >= 7)
        && groups_ok
        && run.iter().any(|x| x.is_ascii_digit())
        && !c.get(j).is_some_and(|x| x.is_alphanumeric())
    {
        let mut e = j;
        while e > i && c[e - 1] == ':' && !double {
            e -= 1;
        }
        return Some(e.max(i + 1));
    }
    None
}

fn match_ipv4(c: &[char], i: usize) -> Option<usize> {
    // The tail of a longer dotted number (1.2.3.4.5) is a version, not an address.
    if i >= 2 && c[i - 1] == '.' && c[i - 2].is_ascii_digit() {
        return None;
    }
    let mut j = i;
    for part in 0..4 {
        let s = j;
        j = digits_end(c, j);
        let n = j - s;
        if n == 0 || n > 3 {
            return None;
        }
        let v: u32 = c[s..j]
            .iter()
            .fold(0, |a, d| a * 10 + d.to_digit(10).unwrap_or(0));
        if v > 255 {
            return None;
        }
        if part < 3 {
            if c.get(j) != Some(&'.') {
                return None;
            }
            j += 1;
        }
    }
    // 1.2.3.4.5 is a version number, not an address.
    if c.get(j) == Some(&'.') && c.get(j + 1).is_some_and(|x| x.is_ascii_digit()) {
        return None;
    }
    if c.get(j).is_some_and(|x| x.is_alphanumeric()) {
        return None;
    }
    Some(tail(c, j))
}

fn match_domain(c: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    while j < c.len() && (c[j].is_alphanumeric() || matches!(c[j], '-' | '.')) {
        j += 1;
    }
    while j > i && matches!(c[j - 1], '.' | '-') {
        j -= 1;
    }
    let run = &c[i..j];
    if !run.contains(&'.') {
        return None;
    }
    let labels: Vec<&[char]> = run.split(|&x| x == '.').collect();
    if labels
        .iter()
        .any(|l| l.is_empty() || l[0] == '-' || l[l.len() - 1] == '-')
    {
        return None;
    }
    let tld: String = labels[labels.len() - 1]
        .iter()
        .collect::<String>()
        .to_lowercase();
    let punycode = tld.starts_with("xn--");
    let alpha_tld = tld.chars().count() >= 2
        && tld.chars().count() <= 24
        && tld.chars().all(|x| x.is_alphabetic());
    if !(alpha_tld || punycode) {
        return None;
    }
    let known = KNOWN_TLDS.contains(&tld.as_str()) || punycode;
    match c.get(j) {
        Some('/') => Some(tail(c, j)),
        Some('?') | Some('#') if c.get(j + 1).is_some_and(|x| x.is_alphanumeric()) => {
            Some(tail(c, j))
        }
        Some(':') if known && c.get(j + 1).is_some_and(|x| x.is_ascii_digit()) => Some(tail(c, j)),
        _ if known => Some(j),
        _ => None,
    }
}

fn match_token(c: &[char], i: usize) -> Option<usize> {
    if i > 0 && is_tok(c[i - 1]) {
        return None;
    }
    let mut j = i;
    while j < c.len() && is_tok(c[j]) {
        j += 1;
    }
    while j > i && c[j - 1] == '.' {
        j -= 1;
    }
    let run = &c[i..j];
    let len = run.len();
    if len < 24 {
        return None;
    }
    if len >= 32 && run.iter().all(|x| x.is_ascii_hexdigit()) {
        return Some(j);
    }
    let classes = usize::from(run.iter().any(|x| x.is_ascii_uppercase()))
        + usize::from(run.iter().any(|x| x.is_ascii_lowercase()))
        + usize::from(run.iter().any(|x| x.is_ascii_digit()));
    (classes >= 2).then_some(j)
}

fn match_at(c: &[char], i: usize) -> Option<(usize, &'static str)> {
    if let Some(e) = match_scheme(c, i)
        .or_else(|| match_special(c, i))
        .or_else(|| match_www(c, i))
        .or_else(|| match_query(c, i))
    {
        return Some((e, "[link]"));
    }
    if let Some(e) = match_email(c, i) {
        return Some((e, "[email]"));
    }
    if let Some(e) = match_localhost(c, i)
        .or_else(|| match_ipv6(c, i))
        .or_else(|| match_ipv4(c, i))
        .or_else(|| match_domain(c, i))
    {
        return Some((e, "[link]"));
    }
    match_token(c, i).map(|e| (e, "[token]"))
}

/// Replace URL-like substrings with `[link]`, long opaque tokens with `[token]` and e-mail
/// addresses with `[email]`. Input is expected to be sanitised already (single-spaced).
/// Idempotent; never panics; output is never longer than the input plus a few characters.
pub(crate) fn scrub_links(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        let boundary = i == 0 || !c[i - 1].is_alphanumeric();
        if boundary {
            if let Some((end, rep)) = match_at(&c, i) {
                if end > i {
                    out.push_str(rep);
                    i = end;
                    continue;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Number of replacement markers in `s` (used to compare two scrubbed views).
pub(crate) fn mark_count(s: &str) -> usize {
    ["[link]", "[token]", "[email]"]
        .iter()
        .map(|m| s.matches(m).count())
        .sum()
}

// ------------------------------------------------------------------ label lexicons

fn words(label: &str) -> Vec<String> {
    label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn has_phrase(w: &[String], phrase: &[&str]) -> bool {
    w.windows(phrase.len())
        .any(|win| win.iter().zip(phrase).all(|(a, b)| a == b))
}

/// Label of an editable node that looks like a browser address / search / location bar.
/// "Email address", "Street address" and similar form labels are not address bars.
pub(crate) fn is_address_bar_label(label: &str) -> bool {
    let w = words(label);
    let has = |x: &str| w.iter().any(|t| t == x);
    if has("omnibox")
        || has("url")
        || has("urls")
        || has("location")
        || has("locations")
        || has("searchbar")
        || has_phrase(&w, &["search", "bar"])
    {
        return true;
    }
    if has("address") || has("addresses") {
        const FORM: &[&str] = &[
            "email",
            "mail",
            "ip",
            "mac",
            "street",
            "shipping",
            "billing",
            "postal",
            "delivery",
            "home",
            "work",
            "contact",
            "wallet",
            "bitcoin",
            "ethereum",
            "crypto",
            "from",
            "to",
            "cc",
            "bcc",
            "recipient",
            "sender",
            "reply",
        ];
        return !w.iter().any(|t| FORM.contains(&t.as_str()));
    }
    false
}

/// Container labels that denote a browser tab strip (case-insensitive).
pub(crate) fn is_tab_strip_label(label: &str) -> bool {
    let w = words(label);
    w.iter()
        .any(|t| matches!(t.as_str(), "tabs" | "tablist" | "tabstrip"))
        || has_phrase(&w, &["tab", "strip"])
        || has_phrase(&w, &["tab", "list"])
        || has_phrase(&w, &["tab", "bar"])
        || has_phrase(&w, &["tab", "row"])
}

/// A toolbar whose name starts with "tab" (e.g. "Tab Search", "Tabs").
pub(crate) fn toolbar_starts_with_tab(label: &str) -> bool {
    words(label).first().is_some_and(|t| t.starts_with("tab"))
}

/// Browser chrome containers holding page titles, URLs and file names: bookmarks bar, downloads
/// shelf / bubble.
pub(crate) fn is_browser_chrome_label(label: &str) -> bool {
    let w = words(label);
    if w.iter().any(|t| t == "bookmarks" || t == "bookmark") {
        return true;
    }
    let dl = w.iter().any(|t| t == "download" || t == "downloads");
    dl && w
        .iter()
        .any(|t| matches!(t.as_str(), "shelf" | "bar" | "bubble" | "toolbar" | "panel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String {
        scrub_links(x)
    }

    #[test]
    fn table() {
        let cases: &[(&str, &str)] = &[
            // schemes
            (
                "https://evil.example/reset?token=SECRET_TOKEN_ABC123",
                "[link]",
            ),
            ("see http://a.b/c now", "see [link] now"),
            ("HTTPS://EVIL.EXAMPLE/X", "[link]"),
            ("ftp://user:pw@host/path", "[link]"),
            ("chrome://settings/passwords", "[link]"),
            ("view-source:https://x.y/z", "view-source:[link]"),
            ("xhttps://x.y", "[link]"),
            ("https://", "[link]"),
            // trailing punctuation stays
            ("go to https://a.example/p.", "go to [link]."),
            ("(https://a.example/p)", "([link])"),
            ("https://a.example/p, then", "[link], then"),
            ("really https://a.example/p?!", "really [link]?!"),
            ("'https://a.example/p'", "'[link]'"),
            // markdown
            ("[click](https://a.example/reset?t=1)", "[click]([link])"),
            ("[https://a.example](https://a.example)", "[[link]]([link])"),
            // balanced parentheses stay inside the URL
            ("https://en.wikipedia.org/wiki/A_(b)", "[link]"),
            // www / mailto / file / localhost
            ("www.evil.example", "[link]"),
            ("WWW.Evil.Example/x", "[link]"),
            ("mailto:a@b.example", "[link]"),
            ("file:///home/u/.ssh/id_rsa", "[link]"),
            ("file:C:/Users/me", "[link]"),
            ("javascript:alert(1)", "[link]"),
            ("localhost:8080/admin", "[link]"),
            ("localhost:3000", "[link]"),
            ("localhost/x", "[link]"),
            // bare domains
            ("evil.example/reset", "[link]"),
            ("evil.example", "[link]"),
            ("example.com", "[link]"),
            ("sub.evil.co/path?q=1#f", "[link]"),
            ("evil.example:8443/x", "[link]"),
            ("login.corp.internal", "[link]"),
            ("shop.example.org?id=7", "[link]"),
            ("unknown.tldx/path", "[link]"),
            // IDN and unicode
            ("https://пример.рф/путь", "[link]"),
            ("пример.com", "[link]"),
            ("münchen.de/x", "[link]"),
            ("xn--e1afmkfd.xn--p1ai", "[link]"),
            ("https://evil.example/\u{202E}x", "[link]"),
            // IPs
            ("10.0.0.1", "[link]"),
            ("192.168.1.10:8080/x", "[link]"),
            ("http://[::1]:8080/x", "[link]"),
            ("[2001:db8::1]:443", "[link]"),
            ("2001:db8::1", "[link]"),
            ("fe80::1", "[link]"),
            // query only
            ("?token=abc", "[link]"),
            ("&session=xyz&b=1", "[link]"),
            ("#access_token=abc", "[link]"),
            // emails
            ("bob@example.com", "[email]"),
            (
                "mail bob.smith+tag@mail.example.org now",
                "mail [email] now",
            ),
            ("(bob@example.com)", "([email])"),
            // tokens
            ("Abcdef0123456789Abcdef0123456789", "[token]"),
            ("ghp_AbCdEf0123456789AbCdEf0123456789", "[token]"),
            ("0123456789abcdef0123456789abcdef", "[token]"),
            ("0123456789abcdef0123456789abcdef01234567", "[token]"),
            ("QWxhZGRpbjpvcGVuIHNlc2FtZQ==QWxhZGRpbjpvcGVu", "[token]"),
            ("key AbCd1234AbCd1234AbCd1234AbCd. end", "key [token]. end"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc",
                "[token]",
            ),
            // NOT scrubbed
            ("e.g. this", "e.g. this"),
            ("i.e. that", "i.e. that"),
            ("etc.", "etc."),
            ("version 1.2.3", "version 1.2.3"),
            ("v10.0.19045.1", "v10.0.19045.1"),
            ("1.2.3.4.5", "1.2.3.4.5"),
            ("999.1.1.1", "999.1.1.1"),
            ("Node.js and file.txt", "Node.js and file.txt"),
            ("main.rs", "main.rs"),
            ("Dr. Smith, Ph.D", "Dr. Smith, Ph.D"),
            ("a.m. to p.m.", "a.m. to p.m."),
            ("Total: 12:30:45", "Total: 12:30:45"),
            ("Sign in", "Sign in"),
            ("Forgot password?", "Forgot password?"),
            ("Add::Bad", "Add::Bad"),
            ("3.14", "3.14"),
            ("aa:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:ff"),
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            ("abcdefghijklmnopqrstuvwxyz", "abcdefghijklmnopqrstuvwxyz"),
            ("1234567890123456789012345", "1234567890123456789012345"),
            ("price $5.99 / month", "price $5.99 / month"),
            ("@username", "@username"),
            ("C#", "C#"),
            ("#3 button", "#3 button"),
            ("Why?", "Why?"),
            ("data: 5", "data: 5"),
            ("file: foo", "file: foo"),
            ("", ""),
            ("   ", "   "),
        ];
        for (input, want) in cases {
            assert_eq!(&s(input), want, "input {input:?}");
        }
        assert!(cases.len() >= 40);
    }

    #[test]
    fn idempotent_and_marks() {
        for (input, _) in [
            ("a https://x.y/z b@c.de AbCd1234AbCd1234AbCd1234AbCd", ()),
            ("[link] [token] [email]", ()),
        ] {
            let once = s(input);
            assert_eq!(s(&once), once);
        }
        assert_eq!(mark_count("[link] x [token] [link]"), 3);
    }

    #[test]
    fn adversarial_inputs_do_not_panic() {
        let mut inputs: Vec<String> = vec![
            "https://".into(),
            "://".into(),
            "[".into(),
            "]".into(),
            "[::".into(),
            "[:::::::::::]".into(),
            "::::::::".into(),
            ":".into(),
            ".".into(),
            "..".into(),
            "...@...".into(),
            "@".into(),
            "a@".into(),
            "@b.cc".into(),
            "localhost".into(),
            "localhost:".into(),
            "www.".into(),
            "mailto:".into(),
            "xn--".into(),
            "?=".into(),
            "-.-".into(),
            "1.1.1.1.".into(),
            "1.1.1.".into(),
            "\u{0}\u{FFFF}\u{10FFFF}".into(),
            "((((((((((((".into(),
            "))))))))))))".into(),
            "𝕙𝕥𝕥𝕡𝕤://𝕖.𝕩".into(),
            "a".repeat(5_000),
            "a.".repeat(2_000),
            "1.".repeat(2_000),
            "http://".repeat(2_000),
            "%".repeat(10_000),
            "é".repeat(10_000) + ".com",
        ];
        // deterministic pseudo-random strings over a hostile alphabet
        let alphabet: Vec<char> = "ab1:./?#@[]()-_=%é\u{202E} wWhHtTpPsS".chars().collect();
        let mut x: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2000 {
            let mut st = String::new();
            for _ in 0..(x % 40) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                st.push(alphabet[(x % alphabet.len() as u64) as usize]);
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            inputs.push(st);
        }
        for i in &inputs {
            let o = scrub_links(i);
            assert_eq!(scrub_links(&o), o, "not idempotent for {i:?}");
        }
    }

    #[test]
    fn lexicons() {
        for l in [
            "Address and search bar",
            "Address bar",
            "Search bar",
            "Omnibox",
            "URL",
            "Location",
            "Location bar",
            "address",
            "Enter URL",
        ] {
            assert!(is_address_bar_label(l), "{l}");
        }
        for l in [
            "Email address",
            "Street address",
            "Username",
            "Search",
            "Password",
            "IP address",
            "",
        ] {
            assert!(!is_address_bar_label(l), "{l}");
        }
        for l in ["Tab strip", "Tabs", "Tab list", "TabList", "tab bar"] {
            assert!(is_tab_strip_label(l), "{l}");
        }
        for l in ["Settings", "New tab button", "Table"] {
            assert!(!is_tab_strip_label(l), "{l}");
        }
        assert!(toolbar_starts_with_tab("Tab Search"));
        assert!(!toolbar_starts_with_tab("Browser tools"));
        for l in [
            "Bookmarks bar",
            "Bookmarks",
            "Downloads shelf",
            "Download bubble",
        ] {
            assert!(is_browser_chrome_label(l), "{l}");
        }
        assert!(!is_browser_chrome_label("Downloads"));
        assert!(!is_browser_chrome_label("Save"));
    }
}
