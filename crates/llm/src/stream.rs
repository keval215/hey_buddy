//! Streaming deltas and the two pure, incremental response parsers (ADR 0002 section 6).
//!
//! Parsers never see headers or secrets, do no I/O, and produce identical output for any
//! chunking of the same bytes. After the first error they stop (fail closed). Error values
//! carry only fixed strings or allowlisted tokens, never response content.

use serde::Deserialize;

use crate::LlmError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmDelta {
    Text(String),
    Done { stop: StopReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    Other,
}

/// Maximum length of one NDJSON line / SSE line / buffered SSE event.
const MAX_LINE_BYTES: usize = 1 << 20;

/// Object-safe view of the parsers for the stream pipeline.
pub(crate) trait DeltaParser: Send {
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<Result<LlmDelta, LlmError>>;
    fn finish_stream(&mut self) -> Vec<Result<LlmDelta, LlmError>>;
}

/// Splits a byte stream into `\n`-terminated lines (trailing `\r` stripped) without ever
/// splitting a UTF-8 sequence (splitting happens on the ASCII byte `\n` only).
#[derive(Default)]
struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    /// Appends `chunk` and returns the completed lines, or `Err` if a line is too long.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, LlmError> {
        let mut lines = Vec::new();
        let mut rest = chunk;
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            self.buf.extend_from_slice(&rest[..pos]);
            if self.buf.len() > MAX_LINE_BYTES {
                return Err(LlmError::Protocol("line_too_long"));
            }
            rest = &rest[pos + 1..];
            lines.push(self.take());
        }
        self.buf.extend_from_slice(rest);
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(LlmError::Protocol("line_too_long"));
        }
        Ok(lines)
    }

    fn take(&mut self) -> Vec<u8> {
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        line
    }

    /// The unterminated last line, if any.
    fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            None
        } else {
            Some(self.take())
        }
    }
}

// ---------------------------------------------------------------------------------------
// Ollama NDJSON
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct OllamaLine {
    #[serde(default)]
    message: Option<OllamaMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

/// Parser for Ollama `/api/chat` streaming (one JSON object per line).
#[derive(Default)]
pub struct OllamaNdjsonParser {
    lines: LineSplitter,
    finished: bool,
}

impl OllamaNdjsonParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn handle_line(&mut self, line: &[u8], out: &mut Vec<Result<LlmDelta, LlmError>>) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let parsed: OllamaLine = match serde_json::from_slice(line) {
            Ok(p) => p,
            Err(_) => return self.fail(LlmError::Protocol("malformed_json"), out),
        };
        if parsed.error.is_some() {
            // Ollama error text is free-form; never forward it.
            return self.fail(LlmError::Provider("unknown".into()), out);
        }
        if let Some(m) = parsed.message {
            if !m.content.is_empty() {
                out.push(Ok(LlmDelta::Text(m.content)));
            }
        }
        if parsed.done {
            let stop = match parsed.done_reason.as_deref() {
                Some("stop") | None => StopReason::EndTurn,
                Some("length") => StopReason::MaxTokens,
                Some(_) => StopReason::Other,
            };
            out.push(Ok(LlmDelta::Done { stop }));
            self.finished = true;
        }
    }

    fn fail(&mut self, e: LlmError, out: &mut Vec<Result<LlmDelta, LlmError>>) {
        out.push(Err(e));
        self.finished = true;
    }

    /// Feed bytes; returns the deltas completed by them.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<LlmDelta, LlmError>> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        match self.lines.push(chunk) {
            Err(e) => self.fail(e, &mut out),
            Ok(lines) => {
                for l in lines {
                    if self.finished {
                        break;
                    }
                    self.handle_line(&l, &mut out);
                }
            }
        }
        out
    }

    /// Signal end of input. Reports `Protocol("stream_ended_without_done")` if no `done`
    /// line was seen.
    pub fn finish(&mut self) -> Vec<Result<LlmDelta, LlmError>> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if let Some(l) = self.lines.flush() {
            self.handle_line(&l, &mut out);
        }
        if !self.finished {
            self.fail(LlmError::Protocol("stream_ended_without_done"), &mut out);
        }
        out
    }
}

impl DeltaParser for OllamaNdjsonParser {
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<Result<LlmDelta, LlmError>> {
        self.push(chunk)
    }
    fn finish_stream(&mut self) -> Vec<Result<LlmDelta, LlmError>> {
        self.finish()
    }
}

// ---------------------------------------------------------------------------------------
// Anthropic SSE
// ---------------------------------------------------------------------------------------

/// Error tokens Anthropic documents; anything else is reported as `unknown`.
const ANTHROPIC_ERROR_ALLOWLIST: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "permission_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "timeout_error",
    "api_error",
    "overloaded_error",
];

#[derive(Deserialize)]
struct SseEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    delta: Option<SseDelta>,
    #[serde(default)]
    error: Option<SseError>,
}

#[derive(Deserialize)]
struct SseDelta {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct SseError {
    #[serde(rename = "type", default)]
    kind: String,
}

/// Parser for Anthropic Messages API server-sent events. The event kind is taken from the
/// JSON `type` field (the `event:` line is ignored); lines are split on `\n` (a trailing
/// `\r` is stripped).
#[derive(Default)]
pub struct AnthropicSseParser {
    lines: LineSplitter,
    data: Vec<u8>,
    has_data: bool,
    stop: Option<StopReason>,
    finished: bool,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn fail(&mut self, e: LlmError, out: &mut Vec<Result<LlmDelta, LlmError>>) {
        out.push(Err(e));
        self.finished = true;
    }

    fn handle_line(&mut self, line: &[u8], out: &mut Vec<Result<LlmDelta, LlmError>>) {
        if line.is_empty() {
            return self.dispatch(out);
        }
        if line[0] == b':' {
            return; // comment / keep-alive
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (line, &line[line.len()..]),
        };
        if field == b"data" {
            if self.has_data {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
            self.has_data = true;
            if self.data.len() > MAX_LINE_BYTES {
                self.fail(LlmError::Protocol("line_too_long"), out);
            }
        }
        // `event:`, `id:`, `retry:` and unknown fields are ignored.
    }

    fn dispatch(&mut self, out: &mut Vec<Result<LlmDelta, LlmError>>) {
        if !self.has_data {
            return;
        }
        let data = std::mem::take(&mut self.data);
        self.has_data = false;
        let ev: SseEvent = match serde_json::from_slice(&data) {
            Ok(e) => e,
            Err(_) => return self.fail(LlmError::Protocol("malformed_json"), out),
        };
        match ev.kind.as_str() {
            "content_block_delta" => {
                if let Some(SseDelta {
                    kind,
                    text: Some(t),
                    ..
                }) = ev.delta
                {
                    if kind == "text_delta" && !t.is_empty() {
                        out.push(Ok(LlmDelta::Text(t)));
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = ev.delta.and_then(|d| d.stop_reason) {
                    self.stop = Some(match r.as_str() {
                        "end_turn" | "stop_sequence" => StopReason::EndTurn,
                        "max_tokens" => StopReason::MaxTokens,
                        _ => StopReason::Other,
                    });
                }
            }
            "message_stop" => {
                out.push(Ok(LlmDelta::Done {
                    stop: self.stop.unwrap_or(StopReason::Other),
                }));
                self.finished = true;
            }
            "error" => {
                let token = ev
                    .error
                    .map(|e| e.kind)
                    .filter(|k| ANTHROPIC_ERROR_ALLOWLIST.contains(&k.as_str()))
                    .unwrap_or_else(|| "unknown".into());
                self.fail(LlmError::Provider(token), out);
            }
            // message_start, ping, content_block_start/stop, unknown future events.
            _ => {}
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<LlmDelta, LlmError>> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        match self.lines.push(chunk) {
            Err(e) => self.fail(e, &mut out),
            Ok(lines) => {
                for l in lines {
                    if self.finished {
                        break;
                    }
                    self.handle_line(&l, &mut out);
                }
            }
        }
        out
    }

    /// Signal end of input: processes an unterminated last line/event, then reports
    /// `Protocol("stream_ended_without_done")` if `message_stop` never arrived.
    pub fn finish(&mut self) -> Vec<Result<LlmDelta, LlmError>> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if let Some(l) = self.lines.flush() {
            self.handle_line(&l, &mut out);
        }
        if !self.finished {
            self.dispatch(&mut out);
        }
        if !self.finished {
            self.fail(LlmError::Protocol("stream_ended_without_done"), &mut out);
        }
        out
    }
}

impl DeltaParser for AnthropicSseParser {
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<Result<LlmDelta, LlmError>> {
        self.push(chunk)
    }
    fn finish_stream(&mut self) -> Vec<Result<LlmDelta, LlmError>> {
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Out = Vec<Result<LlmDelta, LlmError>>;

    fn run_ollama(chunks: &[&[u8]]) -> Out {
        let mut p = OllamaNdjsonParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    fn run_sse(chunks: &[&[u8]]) -> Out {
        let mut p = AnthropicSseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    /// Splits at every single byte offset (two chunks) and also one byte at a time.
    fn assert_split_invariant(input: &[u8], run: fn(&[&[u8]]) -> Out) -> Out {
        let whole = run(&[input]);
        for i in 0..=input.len() {
            let (a, b) = input.split_at(i);
            assert_eq!(run(&[a, b]), whole, "split at {i}");
        }
        let singles: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(run(&singles), whole, "byte at a time");
        whole
    }

    const OLLAMA_OK: &str = concat!(
        "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
        "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"lo \\u00e9\\u4e16\"},\"done\":false}\n",
        "\r\n",
        "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":false}\n",
        "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"!\u{1F600}\"},\"done\":false}\r\n",
        "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\"}\n",
    );

    #[test]
    fn ollama_happy_path_every_split() {
        let out = assert_split_invariant(OLLAMA_OK.as_bytes(), run_ollama);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("Hel".into())),
                Ok(LlmDelta::Text("lo \u{e9}\u{4e16}".into())),
                Ok(LlmDelta::Text("!\u{1F600}".into())),
                Ok(LlmDelta::Done {
                    stop: StopReason::EndTurn
                }),
            ]
        );
    }

    #[test]
    fn ollama_length_and_other_stop_reasons() {
        let l = "{\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"length\"}\n";
        let o = "{\"done\":true,\"done_reason\":\"load\"}";
        assert_eq!(
            assert_split_invariant(l.as_bytes(), run_ollama),
            vec![Ok(LlmDelta::Done {
                stop: StopReason::MaxTokens
            })]
        );
        // Unterminated final line is handled by finish().
        assert_eq!(
            assert_split_invariant(o.as_bytes(), run_ollama),
            vec![Ok(LlmDelta::Done {
                stop: StopReason::Other
            })]
        );
    }

    #[test]
    fn ollama_error_line_is_provider_unknown_and_stops() {
        let input = concat!(
            "{\"message\":{\"content\":\"a\"},\"done\":false}\n",
            "{\"error\":\"model PRIVATE_MARKER_123 not found\"}\n",
            "{\"message\":{\"content\":\"never\"},\"done\":false}\n",
        );
        let out = assert_split_invariant(input.as_bytes(), run_ollama);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("a".into())),
                Err(LlmError::Provider("unknown".into())),
            ]
        );
    }

    #[test]
    fn ollama_malformed_line_is_protocol_error() {
        let input =
            "{\"message\":{\"content\":\"a\"},\"done\":false}\nnot json PRIVATE_MARKER_123\n";
        let out = assert_split_invariant(input.as_bytes(), run_ollama);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("a".into())),
                Err(LlmError::Protocol("malformed_json")),
            ]
        );
        assert!(!format!("{out:?}").contains("PRIVATE_MARKER"));
    }

    #[test]
    fn ollama_missing_done_is_protocol_error() {
        let input = "{\"message\":{\"content\":\"a\"},\"done\":false}\n";
        let out = assert_split_invariant(input.as_bytes(), run_ollama);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("a".into())),
                Err(LlmError::Protocol("stream_ended_without_done")),
            ]
        );
        assert_eq!(
            run_ollama(&[]),
            vec![Err(LlmError::Protocol("stream_ended_without_done"))]
        );
    }

    #[test]
    fn ollama_overlong_line_fails_closed() {
        let mut p = OllamaNdjsonParser::new();
        let big = vec![b'x'; MAX_LINE_BYTES + 1];
        let out = p.push(&big);
        assert_eq!(out, vec![Err(LlmError::Protocol("line_too_long"))]);
        assert!(p.push(b"{}\n").is_empty());
        assert!(p.finish().is_empty());
    }

    const SSE_OK: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"content\":[]}}\n",
        "\n",
        ": keep-alive comment\n",
        "event: content_block_start\r\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n",
        "\r\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"\u{e9}\u{1F600}\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    #[test]
    fn sse_happy_path_every_split() {
        let out = assert_split_invariant(SSE_OK.as_bytes(), run_sse);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("Hi ".into())),
                Ok(LlmDelta::Text("\u{e9}\u{1F600}".into())),
                Ok(LlmDelta::Done {
                    stop: StopReason::MaxTokens
                }),
            ]
        );
    }

    #[test]
    fn sse_multi_line_data_and_unterminated_last_event() {
        // The JSON is split over two `data:` lines (joined with \n, valid JSON whitespace),
        // and the stream ends without the final blank line.
        let input = concat!(
            "data: {\"type\":\"content_block_delta\",\n",
            "data: \"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}",
        );
        let out = assert_split_invariant(input.as_bytes(), run_sse);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("ok".into())),
                Ok(LlmDelta::Done {
                    stop: StopReason::EndTurn
                }),
            ]
        );
    }

    #[test]
    fn sse_error_event_uses_allowlisted_token_or_unknown() {
        let known = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"PRIVATE_MARKER_123\"}}\n\n";
        let unknown = "data: {\"type\":\"error\",\"error\":{\"type\":\"PRIVATE_MARKER_123\",\"message\":\"x\"}}\n\n";
        let none = "data: {\"type\":\"error\"}\n\n";
        for (inp, tok) in [
            (known, "overloaded_error"),
            (unknown, "unknown"),
            (none, "unknown"),
        ] {
            let out = assert_split_invariant(inp.as_bytes(), run_sse);
            assert_eq!(out, vec![Err(LlmError::Provider(tok.into()))]);
            assert!(!format!("{out:?}").contains("PRIVATE_MARKER"));
        }
    }

    #[test]
    fn sse_malformed_and_missing_done() {
        let bad = "data: {nope PRIVATE_MARKER_123\n\n";
        let out = assert_split_invariant(bad.as_bytes(), run_sse);
        assert_eq!(out, vec![Err(LlmError::Protocol("malformed_json"))]);
        let partial =
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}\n\n";
        let out = assert_split_invariant(partial.as_bytes(), run_sse);
        assert_eq!(
            out,
            vec![
                Ok(LlmDelta::Text("a".into())),
                Err(LlmError::Protocol("stream_ended_without_done")),
            ]
        );
    }

    #[test]
    fn sse_stop_without_reason_is_other_and_stops_reading() {
        let input = concat!(
            "data: {\"type\":\"message_stop\"}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n",
        );
        let out = assert_split_invariant(input.as_bytes(), run_sse);
        assert_eq!(
            out,
            vec![Ok(LlmDelta::Done {
                stop: StopReason::Other
            })]
        );
    }
}
