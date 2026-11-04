/// Splits streamed LLM text deltas into sentences so TTS can start before the reply ends.
///
/// A sentence ends at `.`, `!`, `?` or `…` (plus any closing quotes/brackets) followed by
/// whitespace, or at a newline. Decimals like `3.5` do not split.
#[derive(Debug, Default, Clone)]
pub struct SentenceChunker {
    buf: String,
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a delta; returns any completed sentences (trimmed, non-empty).
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(end) = self.find_boundary() {
            let rest = self.buf.split_off(end);
            let sentence = std::mem::replace(&mut self.buf, rest);
            let s = sentence.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
        out
    }

    /// Flush the remaining partial sentence at end of stream.
    pub fn finish(&mut self) -> Option<String> {
        let s = std::mem::take(&mut self.buf);
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Byte index just after a sentence boundary (including the whitespace char), if any.
    fn find_boundary(&self) -> Option<usize> {
        let mut after_term = false;
        for (i, c) in self.buf.char_indices() {
            if c == '\n' {
                return Some(i + 1);
            }
            if matches!(c, '.' | '!' | '?' | '…') {
                after_term = true;
            } else if after_term && matches!(c, '"' | '\'' | ')' | ']' | '”' | '’') {
                // closing punctuation stays attached
            } else if after_term && c.is_whitespace() {
                return Some(i + c.len_utf8());
            } else {
                after_term = false;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_deltas() {
        let mut c = SentenceChunker::new();
        assert!(c.push("Hello the").is_empty());
        assert!(c.push("re.").is_empty()); // no whitespace yet
        assert_eq!(c.push(" How are"), vec!["Hello there."]);
        assert_eq!(c.push(" you? Fine! Ok"), vec!["How are you?", "Fine!"]);
        assert_eq!(c.finish().as_deref(), Some("Ok"));
        assert_eq!(c.finish(), None);
    }

    #[test]
    fn decimals_and_ellipsis() {
        let mut c = SentenceChunker::new();
        assert!(c.push("Pi is 3.14 roughly").is_empty());
        assert_eq!(
            c.push(" ok. Wait... what? "),
            vec!["Pi is 3.14 roughly ok.", "Wait...", "what?"]
        );
        assert_eq!(c.finish(), None);
    }

    #[test]
    fn quotes_and_newlines() {
        let mut c = SentenceChunker::new();
        assert_eq!(
            c.push("He said \"go.\" Then left\nNext"),
            vec!["He said \"go.\"", "Then left"]
        );
        assert_eq!(c.finish().as_deref(), Some("Next"));
    }

    #[test]
    fn whitespace_only_yields_nothing() {
        let mut c = SentenceChunker::new();
        assert!(c.push("  \n \n").is_empty());
        assert_eq!(c.finish(), None);
    }

    #[test]
    fn handles_multibyte() {
        let mut c = SentenceChunker::new();
        assert_eq!(c.push("Héllo… wörld. "), vec!["Héllo…", "wörld."]);
    }
}
