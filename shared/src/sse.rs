//! Minimal incremental Server-Sent-Events parser plus an OpenAI
//! `chat.completion.chunk` payload decoder. Pure code (no I/O) shared by
//! `capability-llm` (reading llama-server's stream) and the coordinator
//! (reading a cloud provider's stream).

use serde::Deserialize;

/// Incremental SSE parser: feed raw bytes as they arrive, get back the
/// complete `data:` payloads. Events are delimited by a blank line; multiple
/// `data:` lines within one event are joined with `\n` per the SSE spec.
/// Comment lines (`:`) and non-`data` fields are ignored. `[DONE]` is
/// returned like any other payload — the caller decides what it means.
#[derive(Default)]
pub struct SseParser {
    buf: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes; returns the `data` payloads of every event that
    /// completed with this feed. Invalid UTF-8 is replaced lossily (payloads
    /// are JSON, so real streams are always UTF-8).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        // Normalise CRLF after appending so pairs split across feeds reunite
        // first; event delimiting below then only considers \n\n.
        if self.buf.contains('\r') {
            self.buf = self.buf.replace("\r\n", "\n");
        }
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find("\n\n") {
            let event: String = self.buf.drain(..pos + 2).collect();
            let mut data_lines = Vec::new();
            for line in event.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
                }
            }
            if !data_lines.is_empty() {
                out.push(data_lines.join("\n"));
            }
        }
        out
    }
}

/// The fields we care about from one OpenAI-style streaming chunk. Everything
/// is optional — providers and llama.cpp builds differ on which chunk carries
/// `usage`, whether `timings` exists, etc.
#[derive(Debug, Default, PartialEq)]
pub struct ParsedChunk {
    pub delta: Option<String>,
    pub role: Option<String>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub error: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawChunk {
    #[serde(default)]
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct RawChoice {
    #[serde(default)]
    delta: RawDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

/// Decode one `data:` payload. Returns `None` when the payload isn't JSON
/// (e.g. `[DONE]` — check for that before calling, or treat `None` as skip).
pub fn parse_openai_chunk(data: &str) -> Option<ParsedChunk> {
    let raw: RawChunk = serde_json::from_str(data).ok()?;
    let mut parsed = ParsedChunk::default();
    if let Some(err) = raw.error {
        parsed.error = Some(
            err.get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| err.to_string()),
        );
    }
    if let Some(choice) = raw.choices.into_iter().next() {
        parsed.delta = choice.delta.content;
        parsed.role = choice.delta.role;
        parsed.finish_reason = choice.finish_reason;
    }
    if let Some(usage) = raw.usage {
        parsed.prompt_tokens = Some(usage.prompt_tokens);
        parsed.completion_tokens = Some(usage.completion_tokens);
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_event_single_feed() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: {\"a\":1}\n\n");
        assert_eq!(out, vec![r#"{"a":1}"#]);
    }

    #[test]
    fn event_split_across_feeds() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"a\"").is_empty());
        assert!(p.feed(b":1}").is_empty());
        let out = p.feed(b"\n\n");
        assert_eq!(out, vec![r#"{"a":1}"#]);
    }

    #[test]
    fn multiple_events_one_feed() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: 1\n\ndata: 2\n\ndata: [DONE]\n\n");
        assert_eq!(out, vec!["1", "2", "[DONE]"]);
    }

    #[test]
    fn crlf_delimiters() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: 1\r\n\r\ndata: 2\r\n\r\n");
        assert_eq!(out, vec!["1", "2"]);
    }

    #[test]
    fn multi_data_lines_joined() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: line1\ndata: line2\n\n");
        assert_eq!(out, vec!["line1\nline2"]);
    }

    #[test]
    fn comments_and_fields_ignored() {
        let mut p = SseParser::new();
        let out = p.feed(b": keep-alive\n\nevent: message\ndata: 1\nid: 7\n\n");
        assert_eq!(out, vec!["1"]);
    }

    #[test]
    fn parse_delta_chunk() {
        let c =
            parse_openai_chunk(r#"{"choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#)
                .unwrap();
        assert_eq!(c.delta.as_deref(), Some("hel"));
        assert!(c.finish_reason.is_none());
    }

    #[test]
    fn parse_role_chunk() {
        let c = parse_openai_chunk(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).unwrap();
        assert_eq!(c.role.as_deref(), Some("assistant"));
        assert!(c.delta.is_none());
    }

    #[test]
    fn parse_finish_chunk_with_usage() {
        let c = parse_openai_chunk(
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":42}}"#,
        )
        .unwrap();
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
        assert_eq!(c.prompt_tokens, Some(7));
        assert_eq!(c.completion_tokens, Some(42));
    }

    #[test]
    fn parse_usage_only_chunk() {
        // OpenAI's include_usage sends a chunk with empty choices.
        let c = parse_openai_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":5}}"#,
        )
        .unwrap();
        assert!(c.delta.is_none());
        assert_eq!(c.completion_tokens, Some(5));
    }

    #[test]
    fn parse_error_chunk() {
        let c =
            parse_openai_chunk(r#"{"error":{"message":"boom","type":"server_error"}}"#).unwrap();
        assert_eq!(c.error.as_deref(), Some("boom"));
    }

    #[test]
    fn parse_done_and_garbage_return_none() {
        assert!(parse_openai_chunk("[DONE]").is_none());
        assert!(parse_openai_chunk("not json").is_none());
    }
}
