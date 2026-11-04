//! English text frontend for Kokoro: normalise -> phonemise -> token ids. Feature `tts-kokoro`.
//!
//! No GPL code or data (D-002/D-004). The phonemiser is:
//! - the CMU Pronouncing Dictionary (BSD-2-style licence, ~3.5 MB, too large to embed), loaded
//!   from `<models dir>/cmudict.dict` (an optional user download, `ModelMissing` if absent);
//! - a small built-in letter-to-sound rule set for words not in the dictionary;
//! - letter spelling for short ALL-CAPS words (acronyms).
//!
//! ARPAbet phones are mapped to the Kokoro (misaki-style American English) symbols, and symbols
//! to ids with the `vocab` table read from the model's `config.json` (never hardcoded here).
//! Quality is below an espeak-ng frontend: no heteronym disambiguation, no phrase-level
//! reductions, stress marks go before the vowel rather than the syllable onset.
//!
//! Voice style: Kokoro wants one 256-float style row per phoneme-token count. A voice file is
//! raw little-endian f32, `rows x 256`, at `<models dir>/kokoro-voices/<voice>.bin`
//! (`ModelMissing` naming the file if absent). Row = number of phoneme tokens (without the two
//! pad tokens), clamped to the last row.
//!
//! # Known limits (read before trusting the output)
//!
//! - Phoneme symbols that are not in the model's `vocab` are silently dropped (no error, no log).
//! - Input over [`MAX_PHONEME_TOKENS`] (510) phoneme tokens is truncated at the last word break
//!   that fits; the tail is NOT spoken. `StreamingTts` re-splits text only at sentence ends
//!   (`.`, `!`, `?`, newline), so it does not protect against one very long sentence without
//!   terminators; such a sentence is cut.
//! - Non-ASCII letters in words: common accented Latin letters are mapped to their ASCII base
//!   letter ("café" -> "cafe", "naïve" -> "naive", see `ascii_base`); any other non-ASCII letter
//!   (Cyrillic, Greek, CJK, ß, ...) acts as a word boundary and is not spoken.
//! - The `voice` argument of `TtsCore::synth` is ignored: the voice is fixed when the model is
//!   loaded.
//! - VERIFY (unchecked against a real download): the voice-file layout. This code expects
//!   `<models dir>/kokoro-voices/<voice>.bin` as raw little-endian f32, `[510, 1, 256]` (the
//!   onnx-community style `voices/<name>.bin`, renamed dir). It will NOT load `.pt` voices or the
//!   kokoro-onnx `voices-v1.0.bin` / npz files.
//! - VERIFY: the ARPAbet -> misaki symbol mapping and the `kokoro-82m.config.json` vocab file
//!   name are unverified against the real model.
//!
//! Nothing here logs or puts text in errors.

use crate::text_norm::normalize;
use crate::tts_kokoro::{Phonemizer, STYLE_LEN};
use crate::VoiceError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CMUDICT_FILE: &str = "cmudict.dict";
pub const KOKORO_CONFIG_EXT: &str = "config.json";
pub const VOICES_DIR: &str = "kokoro-voices";
/// Kokoro takes at most 510 phoneme tokens between the two pad tokens.
pub const MAX_PHONEME_TOKENS: usize = 510;

fn backend(what: &str) -> VoiceError {
    VoiceError::Backend(format!("kokoro frontend: {what}"))
}

fn read_file(path: &Path) -> Result<Vec<u8>, VoiceError> {
    if !path.is_file() {
        return Err(VoiceError::ModelMissing {
            path: path.to_path_buf(),
        });
    }
    std::fs::read(path).map_err(|_| backend("cannot read file"))
}

// ---------------------------------------------------------------- lexicon

/// Word -> ARPAbet phones (with stress digits), first pronunciation only.
#[derive(Debug, Default, Clone)]
pub struct Lexicon {
    map: HashMap<String, String>,
}

impl Lexicon {
    /// Parse `cmudict.dict` / `cmudict-0.7b` text (`word PH PH ...`, `;;;` or `#` comments,
    /// `word(2)` variants ignored). Malformed lines are skipped.
    pub fn parse(text: &str) -> Self {
        let mut map = HashMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with(";;;") {
                continue;
            }
            let Some((w, ph)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            if w.contains('(') {
                continue;
            }
            map.entry(w.to_lowercase())
                .or_insert_with(|| ph.trim().to_string());
        }
        Self { map }
    }

    pub fn from_path(path: &Path) -> Result<Self, VoiceError> {
        // cmudict-0.7b is Latin-1; lossy decoding only affects odd entries.
        let bytes = read_file(path)?;
        let lex = Self::parse(&String::from_utf8_lossy(&bytes));
        if lex.map.is_empty() {
            return Err(backend("empty dictionary"));
        }
        Ok(lex)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn get(&self, w: &str) -> Option<Vec<String>> {
        self.map
            .get(w)
            .map(|p| p.split_whitespace().map(str::to_string).collect())
    }

    /// Phones for one word (lowercase letters/apostrophes): dictionary, then suffix handling,
    /// then spelling for acronyms, then letter-to-sound rules.
    pub fn word_phones(&self, original: &str) -> Vec<String> {
        let w = original.to_ascii_lowercase();
        if let Some(p) = self.get(&w) {
            return p;
        }
        for suffix in ["'s", "s"] {
            if let Some(base) = w.strip_suffix(suffix) {
                if base.len() >= 2 {
                    if let Some(mut p) = self.get(base) {
                        let add: &[&str] =
                            match p.last().map(|s| s.trim_end_matches(char::is_numeric)) {
                                Some("P" | "T" | "K" | "F" | "TH") => &["S"],
                                Some("S" | "Z" | "SH" | "ZH" | "CH" | "JH") => &["IH0", "Z"],
                                _ => &["Z"],
                            };
                        p.extend(add.iter().map(|s| s.to_string()));
                        return p;
                    }
                }
            }
        }
        let letters: String = original.chars().filter(char::is_ascii_alphabetic).collect();
        if (2..=5).contains(&letters.len()) && letters.chars().all(|c| c.is_ascii_uppercase()) {
            return letters
                .chars()
                .flat_map(|c| spell_letter(c.to_ascii_lowercase()).split(' '))
                .map(str::to_string)
                .collect();
        }
        rule_g2p(&w)
    }
}

fn spell_letter(c: char) -> &'static str {
    match c {
        'a' => "EY1",
        'b' => "B IY1",
        'c' => "S IY1",
        'd' => "D IY1",
        'e' => "IY1",
        'f' => "EH1 F",
        'g' => "JH IY1",
        'h' => "EY1 CH",
        'i' => "AY1",
        'j' => "JH EY1",
        'k' => "K EY1",
        'l' => "EH1 L",
        'm' => "EH1 M",
        'n' => "EH1 N",
        'o' => "OW1",
        'p' => "P IY1",
        'q' => "K Y UW1",
        'r' => "AA1 R",
        's' => "EH1 S",
        't' => "T IY1",
        'u' => "Y UW1",
        'v' => "V IY1",
        'w' => "D AH1 B AH0 L Y UW0",
        'x' => "EH1 K S",
        'y' => "W AY1",
        _ => "Z IY1",
    }
}

// ---------------------------------------------------------------- letter-to-sound fallback

fn is_vowel(c: u8) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u')
}

/// Crude English letter-to-sound rules for out-of-dictionary words. Intelligible, not exact.
fn rule_g2p(word: &str) -> Vec<String> {
    let mut w: Vec<u8> = word.bytes().filter(|b| b.is_ascii_lowercase()).collect();
    if w.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut long_at: Option<usize> = None;
    let n = w.len();
    // consonant + "le": syllabic L
    let mut tail: Vec<String> = Vec::new();
    if n >= 3 && w.ends_with(b"le") && !is_vowel(w[n - 3]) {
        w.truncate(n - 2);
        tail = vec!["AH0".into(), "L".into()];
        let m = w.len();
        if m >= 2 && is_vowel(w[m - 2]) && w[m - 2] != b'e' {
            long_at = Some(m - 2);
        }
    } else if n >= 3
        && w[n - 1] == b'e'
        && !is_vowel(w[n - 2])
        && is_vowel(w[n - 3])
        && w[n - 3] != b'e'
    {
        // magic e: the vowel before the final consonant is long, the e is silent
        long_at = Some(n - 3);
        w.truncate(n - 1);
    }
    let len = w.len();
    let mut i = 0;
    let at = |i: usize, s: &[u8]| w.get(i..i + s.len()) == Some(s);
    let vowel = |ph: &[&str], out: &mut Vec<String>| {
        for p in ph {
            // stress digits are fixed up by `fix_stress`; the glide Y carries none
            out.push(if *p == "Y" {
                "Y".to_string()
            } else {
                format!("{p}1")
            });
        }
    };
    while i < len {
        let c = w[i];
        let next = w.get(i + 1).copied().unwrap_or(0);
        let mut step = 1;
        match c {
            b't' if at(i, b"tch") => {
                out.push("CH".into());
                step = 3;
            }
            b'c' if at(i, b"ch") => {
                out.push("CH".into());
                step = 2;
            }
            b's' if at(i, b"sh") => {
                out.push("SH".into());
                step = 2;
            }
            b't' if at(i, b"th") => {
                out.push("TH".into());
                step = 2;
            }
            b'p' if at(i, b"ph") => {
                out.push("F".into());
                step = 2;
            }
            b'w' if at(i, b"wh") => {
                out.push("W".into());
                step = 2;
            }
            b'c' if at(i, b"ck") => {
                out.push("K".into());
                step = 2;
            }
            b'n' if at(i, b"ng") => {
                out.push("NG".into());
                step = 2;
            }
            b'g' if at(i, b"gh") => {
                if i + 2 >= len || w[i + 2] == b't' {
                    // silent ("night", "high")
                } else {
                    out.push("G".into());
                }
                step = 2;
            }
            b'q' if at(i, b"qu") => {
                out.extend(["K".into(), "W".into()]);
                step = 2;
            }
            b'k' if i == 0 && next == b'n' => {}
            b'w' if i == 0 && next == b'r' => {}
            b'g' if i == 0 && next == b'n' => {}
            b'm' if at(i, b"mb") && i + 2 == len => {
                out.push("M".into());
                step = 2;
            }
            b'b' => out.push("B".into()),
            b'c' => out.push(
                if matches!(next, b'e' | b'i' | b'y') {
                    "S"
                } else {
                    "K"
                }
                .into(),
            ),
            b'd' => out.push("D".into()),
            b'f' => out.push("F".into()),
            b'g' => out.push(
                if matches!(next, b'e' | b'i' | b'y') {
                    "JH"
                } else {
                    "G"
                }
                .into(),
            ),
            b'h' => out.push("HH".into()),
            b'j' => out.push("JH".into()),
            b'k' => out.push("K".into()),
            b'l' => out.push("L".into()),
            b'm' => out.push("M".into()),
            b'n' => out.push("N".into()),
            b'p' => out.push("P".into()),
            b'q' => out.push("K".into()),
            b'r' => out.push("R".into()),
            b's' => {
                let voiced_before = i > 0
                    && (is_vowel(w[i - 1])
                        || matches!(
                            w[i - 1],
                            b'b' | b'd' | b'g' | b'l' | b'm' | b'n' | b'r' | b'v'
                        ));
                out.push(
                    if i + 1 == len && i > 1 && voiced_before {
                        "Z"
                    } else {
                        "S"
                    }
                    .into(),
                );
            }
            b't' => out.push("T".into()),
            b'v' => out.push("V".into()),
            b'w' => out.push("W".into()),
            b'x' => out.extend(["K".into(), "S".into()]),
            b'z' => out.push("Z".into()),
            b'y' => {
                if i == 0 || is_vowel(next) {
                    out.push("Y".into());
                } else if i + 1 == len && len > 3 {
                    vowel(&["IY"], &mut out);
                } else {
                    vowel(&["AY"], &mut out);
                }
            }
            _ => {
                // vowels
                let long = long_at == Some(i);
                let two = |s: &[u8]| at(i, s);
                if two(b"ee") || two(b"ea") || two(b"ie") {
                    vowel(&["IY"], &mut out);
                    step = 2;
                } else if two(b"oo") || two(b"ue") || two(b"ui") {
                    vowel(&["UW"], &mut out);
                    step = 2;
                } else if two(b"ai") || two(b"ay") || two(b"ei") || two(b"ey") {
                    vowel(&["EY"], &mut out);
                    step = 2;
                } else if two(b"oa") {
                    vowel(&["OW"], &mut out);
                    step = 2;
                } else if two(b"ow") {
                    vowel(&[if i + 2 >= len { "OW" } else { "AW" }], &mut out);
                    step = 2;
                } else if two(b"ou") {
                    vowel(&["AW"], &mut out);
                    step = 2;
                } else if two(b"oi") || two(b"oy") {
                    vowel(&["OY"], &mut out);
                    step = 2;
                } else if two(b"au") || two(b"aw") {
                    vowel(&["AO"], &mut out);
                    step = 2;
                } else if two(b"eu") || two(b"ew") {
                    vowel(&["Y", "UW"], &mut out);
                    step = 2;
                } else if two(b"ar") {
                    vowel(&["AA"], &mut out);
                } else if two(b"or") {
                    vowel(&["AO"], &mut out);
                } else if matches!(c, b'e' | b'i' | b'u') && next == b'r' {
                    vowel(&["ER"], &mut out);
                    step = 2;
                } else {
                    let ph: &[&str] = match (c, long) {
                        (b'a', true) => &["EY"],
                        (b'a', _) => &["AE"],
                        (b'e', true) => &["IY"],
                        (b'e', _) if len == 1 => &["IY"],
                        (b'e', _) => &["EH"],
                        (b'i', true) => &["AY"],
                        (b'i', _) => &["IH"],
                        (b'o', true) => &["OW"],
                        (b'o', _) => &["AA"],
                        (b'u', true) => &["Y", "UW"],
                        (b'u', _) => &["AH"],
                        _ => &[],
                    };
                    vowel(ph, &mut out);
                }
            }
        }
        // collapse doubled consonants ("ll", "tt")
        if step == 1 && !is_vowel(c) && next == c && c != b'c' {
            step = 2;
        }
        i += step;
    }
    out.extend(tail);
    fix_stress(&mut out);
    out
}

/// First vowel gets primary stress, later vowels none.
fn fix_stress(ph: &mut [String]) {
    let mut seen = false;
    for p in ph.iter_mut() {
        if p.ends_with(['0', '1', '2']) {
            let base = p[..p.len() - 1].to_string();
            *p = format!("{base}{}", if seen { "0" } else { "1" });
            seen = true;
        }
    }
}

// ---------------------------------------------------------------- ARPAbet -> Kokoro symbols

/// Kokoro (American English) symbol string for one ARPAbet phone, stress mark first.
pub fn arpa_to_kokoro(phone: &str) -> Option<String> {
    let stress = phone.chars().last().filter(char::is_ascii_digit);
    let base = phone.trim_end_matches(|c: char| c.is_ascii_digit());
    let s = match base {
        "AA" => "ɑ",
        "AE" => "æ",
        "AH" => {
            if stress == Some('0') {
                "ə"
            } else {
                "ʌ"
            }
        }
        "AO" => "ɔ",
        "AW" => "W",
        "AY" => "I",
        "B" => "b",
        "CH" => "ʧ",
        "D" => "d",
        "DH" => "ð",
        "EH" => "ɛ",
        "ER" => {
            if stress == Some('0') {
                "ɚ"
            } else {
                "ɜɹ"
            }
        }
        "EY" => "A",
        "F" => "f",
        "G" => "ɡ",
        "HH" => "h",
        "IH" => "ɪ",
        "IY" => "i",
        "JH" => "ʤ",
        "K" => "k",
        "L" => "l",
        "M" => "m",
        "N" => "n",
        "NG" => "ŋ",
        "OW" => "O",
        "OY" => "Y",
        "P" => "p",
        "R" => "ɹ",
        "S" => "s",
        "SH" => "ʃ",
        "T" => "t",
        "TH" => "θ",
        "UH" => "ʊ",
        "UW" => "u",
        "V" => "v",
        "W" => "w",
        "Y" => "j",
        "Z" => "z",
        "ZH" => "ʒ",
        _ => return None,
    };
    let mark = match stress {
        Some('1') => "ˈ",
        Some('2') => "ˌ",
        _ => "",
    };
    Some(format!("{mark}{s}"))
}

// ---------------------------------------------------------------- vocab, voices

/// Symbol -> token id, from the `vocab` object of Kokoro's `config.json`.
#[derive(Debug, Clone)]
pub struct Vocab {
    map: HashMap<char, i64>,
}

impl Vocab {
    pub fn from_config_json(json: &str) -> Result<Self, VoiceError> {
        let v: serde_json::Value =
            serde_json::from_str(json).map_err(|_| backend("config is not valid JSON"))?;
        let obj = v
            .get("vocab")
            .and_then(|o| o.as_object())
            .ok_or_else(|| backend("config has no vocab"))?;
        let mut map = HashMap::new();
        for (k, id) in obj {
            let mut cs = k.chars();
            if let (Some(c), None, Some(id)) = (cs.next(), cs.next(), id.as_i64()) {
                map.insert(c, id);
            }
        }
        if map.is_empty() {
            return Err(backend("config vocab is empty"));
        }
        Ok(Self { map })
    }

    pub fn from_path(path: &Path) -> Result<Self, VoiceError> {
        let bytes = read_file(path)?;
        Self::from_config_json(&String::from_utf8_lossy(&bytes))
    }

    pub fn id(&self, c: char) -> Option<i64> {
        self.map.get(&c).copied()
    }
}

/// Per-token-count style vectors for one voice.
#[derive(Debug, Clone)]
pub struct VoicePack {
    data: Vec<f32>,
}

impl VoicePack {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VoiceError> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4 * STYLE_LEN) {
            return Err(backend("voice file has an unexpected size"));
        }
        let data: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if data.iter().any(|v| !v.is_finite()) {
            return Err(backend("voice file has invalid values"));
        }
        Ok(Self { data })
    }

    pub fn path(models_dir: &Path, voice: &str) -> Result<PathBuf, VoiceError> {
        if voice.is_empty()
            || !voice
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(backend("invalid voice name"));
        }
        Ok(models_dir.join(VOICES_DIR).join(format!("{voice}.bin")))
    }

    pub fn load(models_dir: &Path, voice: &str) -> Result<Self, VoiceError> {
        Self::from_bytes(&read_file(&Self::path(models_dir, voice)?)?)
    }

    pub fn rows(&self) -> usize {
        self.data.len() / STYLE_LEN
    }

    /// Style row for a sentence with `n_tokens` phoneme tokens (pads excluded).
    pub fn style_for(&self, n_tokens: usize) -> &[f32] {
        let row = n_tokens.min(self.rows() - 1);
        &self.data[row * STYLE_LEN..(row + 1) * STYLE_LEN]
    }
}

// ---------------------------------------------------------------- the frontend

/// normalise -> phonemise -> `[0, ids..., 0]`.
pub struct KokoroFrontend {
    lexicon: Lexicon,
    vocab: Vocab,
}

impl KokoroFrontend {
    pub fn new(lexicon: Lexicon, vocab: Vocab) -> Self {
        Self { lexicon, vocab }
    }

    /// Load the dictionary and vocab from the models dir (`ModelMissing` names the file).
    pub fn from_models_dir(
        registry: &crate::registry::Registry,
        models_dir: &Path,
    ) -> Result<Self, VoiceError> {
        let cfg = models_dir.join(format!(
            "{}.{KOKORO_CONFIG_EXT}",
            registry.get(crate::tts_kokoro::KOKORO_MODEL_ID)?.id
        ));
        Ok(Self::new(
            Lexicon::from_path(&models_dir.join(CMUDICT_FILE))?,
            Vocab::from_path(&cfg)?,
        ))
    }

    /// Phoneme symbols for already-normalised text (exposed for tests).
    pub fn phonemes(&self, normalized: &str) -> String {
        let mut out = String::new();
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String, lex: &Lexicon| {
            if word.is_empty() {
                return;
            }
            let w = word.trim_matches('\'');
            if !w.is_empty() {
                for p in lex.word_phones(w) {
                    if let Some(s) = arpa_to_kokoro(&p) {
                        out.push_str(&s);
                    }
                }
            }
            word.clear();
        };
        for c in normalized.chars() {
            if c == '\'' || c.is_ascii_alphabetic() {
                word.push(c);
            } else if let Some(b) = ascii_base(c) {
                word.push(b);
            } else {
                flush(&mut word, &mut out, &self.lexicon);
                if c.is_whitespace() || c == '-' || c.is_alphabetic() {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                } else if ".,;:!?".contains(c) {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push(c);
                    out.push(' ');
                }
            }
        }
        flush(&mut word, &mut out, &self.lexicon);
        out.trim().to_string()
    }
}

/// ASCII base letter for common accented Latin letters (Latin-1 Supplement, Latin Extended-A).
/// Any other non-ASCII letter is not in this table and acts as a word boundary.
fn ascii_base(c: char) -> Option<char> {
    let b = match c {
        '\u{C0}'..='\u{C5}' | '\u{E0}'..='\u{E5}' | '\u{100}'..='\u{105}' => 'a',
        '\u{C7}' | '\u{E7}' | '\u{106}'..='\u{10D}' => 'c',
        '\u{10E}'..='\u{111}' => 'd',
        '\u{C8}'..='\u{CB}' | '\u{E8}'..='\u{EB}' | '\u{112}'..='\u{11B}' => 'e',
        '\u{11C}'..='\u{123}' => 'g',
        '\u{124}'..='\u{127}' => 'h',
        '\u{CC}'..='\u{CF}' | '\u{EC}'..='\u{EF}' | '\u{128}'..='\u{131}' => 'i',
        '\u{134}' | '\u{135}' => 'j',
        '\u{136}' | '\u{137}' => 'k',
        '\u{139}'..='\u{142}' => 'l',
        '\u{D1}' | '\u{F1}' | '\u{143}'..='\u{148}' => 'n',
        '\u{D2}'..='\u{D6}' | '\u{D8}' | '\u{F2}'..='\u{F6}' | '\u{F8}' | '\u{14C}'..='\u{151}' => {
            'o'
        }
        '\u{154}'..='\u{159}' => 'r',
        '\u{15A}'..='\u{161}' => 's',
        '\u{162}'..='\u{167}' => 't',
        '\u{D9}'..='\u{DC}' | '\u{F9}'..='\u{FC}' | '\u{168}'..='\u{173}' => 'u',
        '\u{174}' | '\u{175}' => 'w',
        '\u{DD}' | '\u{FD}' | '\u{FF}' | '\u{176}'..='\u{178}' => 'y',
        '\u{179}'..='\u{17E}' => 'z',
        _ => return None,
    };
    // Keep the case: upper-case letters sit in the even code points of these blocks only
    // loosely, so decide from the char itself.
    Some(if c.is_uppercase() {
        b.to_ascii_uppercase()
    } else {
        b
    })
}

impl Phonemizer for KokoroFrontend {
    /// Returns `[]` when nothing is speakable (the caller skips synthesis).
    fn tokens(&self, sentence: &str) -> Result<Vec<i64>, VoiceError> {
        let ph = self.phonemes(&normalize(sentence));
        let mut ids: Vec<i64> = ph.chars().filter_map(|c| self.vocab.id(c)).collect();
        if ids.is_empty() || !ph.chars().any(|c| c.is_alphabetic()) {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_PHONEME_TOKENS {
            // Cut at the last word break that fits rather than mid-word.
            let space = self.vocab.id(' ');
            let cut = ids[..MAX_PHONEME_TOKENS]
                .iter()
                .rposition(|id| Some(*id) == space)
                .unwrap_or(MAX_PHONEME_TOKENS);
            ids.truncate(cut);
        }
        let mut t = Vec::with_capacity(ids.len() + 2);
        t.push(0);
        t.extend(ids);
        t.push(0);
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = ";;; mini lexicon\n\
        hello HH AH0 L OW1\n\
        world W ER1 L D\n\
        twenty T W EH1 N T IY0\n\
        four F AO1 R\n\
        dollars D AA1 L ER0 Z\n\
        dollar D AA1 L ER0\n\
        cat K AE1 T\n\
        cat(2) K AE2 T\n\
        dog D AO1 G # comment\n\
        a AH0\n";

    fn vocab_json() -> String {
        // Stand-in for the model's config.json: symbols map to arbitrary distinct ids.
        let syms = "ɑæʌəɔWIbʧdðɛɚɜɹAfɡhɪiʤklmnŋOYpsʃtθʊuvwjzʒˈˌ ;:,.!?";
        let mut m = serde_json::Map::new();
        for (i, c) in syms.chars().enumerate() {
            m.insert(c.to_string(), serde_json::json!(i + 1));
        }
        serde_json::json!({ "vocab": m }).to_string()
    }

    fn frontend() -> KokoroFrontend {
        KokoroFrontend::new(
            Lexicon::parse(MINI),
            Vocab::from_config_json(&vocab_json()).unwrap(),
        )
    }

    #[test]
    fn accented_letters_transliterate_and_others_break_words() {
        let f = frontend();
        assert_eq!(f.phonemes("cafe"), f.phonemes("caf\u{e9}"));
        assert_eq!(f.phonemes("naive"), f.phonemes("na\u{ef}ve"));
        assert_eq!(f.phonemes("CAFE"), f.phonemes("CAF\u{c9}"));
        assert_eq!(f.phonemes("cat"), f.phonemes("c\u{e0}t"));
        assert_eq!(f.phonemes("canon"), f.phonemes("ca\u{f1}on"));
        // Not in the table: boundary, so "cat" and "dog" stay separate words.
        assert_eq!(f.phonemes("cat\u{43a}dog"), f.phonemes("cat dog"));
        assert_eq!(f.phonemes("cat\u{df}dog"), f.phonemes("cat dog"));
        for c in [
            '\u{c0}', '\u{ff}', '\u{100}', '\u{17e}', '\u{130}', '\u{131}',
        ] {
            assert!(ascii_base(c).is_some());
        }
        assert_eq!(ascii_base('\u{d7}'), None);
        assert_eq!(ascii_base('\u{df}'), None);
    }

    #[test]
    fn lexicon_parses_comments_variants_and_lowercases() {
        let l = Lexicon::parse(MINI);
        assert_eq!(l.len(), 9);
        assert_eq!(l.word_phones("Cat"), ["K", "AE1", "T"]);
        assert_eq!(l.word_phones("dog"), ["D", "AO1", "G"]);
    }

    #[test]
    fn known_words_and_numbers() {
        let f = frontend();
        assert_eq!(f.phonemes("hello world"), "həlˈO wˈɜɹld");
        // normalise first: "$4" -> "four dollars"
        assert_eq!(f.phonemes(&normalize("$4")), "fˈɔɹ dˈɑlɚz");
    }

    #[test]
    fn plural_possessive_and_acronym_fallbacks() {
        let f = frontend();
        assert_eq!(f.lexicon.word_phones("cats"), ["K", "AE1", "T", "S"]);
        assert_eq!(f.lexicon.word_phones("dog's"), ["D", "AO1", "G", "Z"]);
        assert_eq!(
            f.lexicon.word_phones("NASA"),
            ["EH1", "N", "EY1", "EH1", "S", "EY1"]
        );
    }

    #[test]
    fn rules_give_plausible_phones_for_unknown_words() {
        let l = Lexicon::default();
        assert_eq!(l.word_phones("make"), ["M", "EY1", "K"]);
        assert_eq!(l.word_phones("ship"), ["SH", "IH1", "P"]);
        assert_eq!(l.word_phones("table"), ["T", "EY1", "B", "AH0", "L"]);
        assert!(l.word_phones("xqzv").len() >= 3);
        assert!(l.word_phones("").is_empty());
    }

    #[test]
    fn tokens_have_pads_and_only_vocab_ids() {
        let f = frontend();
        let t = f.tokens("Hello, world!").unwrap();
        assert_eq!((t[0], *t.last().unwrap()), (0, 0));
        assert!(t.len() > 8);
        // nothing speakable -> empty, not an error
        assert!(f.tokens("🎉🎉").unwrap().is_empty());
        assert!(f.tokens("").unwrap().is_empty());
        assert!(f.tokens("... !!").unwrap().is_empty());
    }

    #[test]
    fn long_input_is_cut_at_a_word_break() {
        let f = frontend();
        let long = "hello world ".repeat(200);
        let t = f.tokens(&long).unwrap();
        assert!(t.len() <= MAX_PHONEME_TOKENS + 2);
        assert!(t.len() > 400);
    }

    #[test]
    fn arpa_mapping_covers_cmudict_phones() {
        for p in "AA AE AH AO AW AY B CH D DH EH ER EY F G HH IH IY JH K L M N NG OW OY P R S SH T TH UH UW V W Y Z ZH"
            .split(' ')
        {
            assert!(arpa_to_kokoro(&format!("{p}1")).is_some(), "{p}");
        }
        assert_eq!(arpa_to_kokoro("AH0").unwrap(), "ə");
        assert_eq!(arpa_to_kokoro("AH1").unwrap(), "ˈʌ");
        assert!(arpa_to_kokoro("XX").is_none());
    }

    #[test]
    fn vocab_errors_are_fixed_strings() {
        assert!(Vocab::from_config_json("not json").is_err());
        assert!(Vocab::from_config_json("{}").is_err());
        assert!(Vocab::from_config_json(r#"{"vocab":{}}"#).is_err());
        let e = Vocab::from_config_json(r#"{"secret":1}"#)
            .unwrap_err()
            .to_string();
        assert!(!e.contains("secret"));
    }

    #[test]
    fn missing_files_are_model_missing_with_the_path() {
        let dir = Path::new("/nonexistent/models");
        let p = dir.join(CMUDICT_FILE);
        assert!(
            matches!(Lexicon::from_path(&p), Err(VoiceError::ModelMissing { path }) if path == p)
        );
        let p = dir.join(VOICES_DIR).join("af_heart.bin");
        assert!(
            matches!(VoicePack::load(dir, "af_heart"), Err(VoiceError::ModelMissing { path }) if path == p)
        );
        assert!(VoicePack::load(dir, "../x").is_err());
        assert!(VoicePack::load(dir, "").is_err());
    }

    #[test]
    fn voice_pack_rows_and_clamping() {
        let mut bytes = Vec::new();
        for row in 0..3 {
            for _ in 0..STYLE_LEN {
                bytes.extend_from_slice(&(row as f32).to_le_bytes());
            }
        }
        let v = VoicePack::from_bytes(&bytes).unwrap();
        assert_eq!(v.rows(), 3);
        assert_eq!(v.style_for(1)[0], 1.0);
        assert_eq!(v.style_for(500)[STYLE_LEN - 1], 2.0);
        assert!(VoicePack::from_bytes(&bytes[..bytes.len() - 4]).is_err());
        assert!(VoicePack::from_bytes(&[]).is_err());
        let mut bad = bytes.clone();
        bad[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(VoicePack::from_bytes(&bad).is_err());
    }

    #[test]
    fn frontend_never_panics_on_odd_input() {
        let f = frontend();
        for s in [
            "'",
            "''''",
            "a'b'c",
            "ÅÄÖ ñ 漢字",
            "\u{0}\u{202e}",
            "x".repeat(5000).as_str(),
        ] {
            let _ = f.tokens(s);
        }
    }
}
