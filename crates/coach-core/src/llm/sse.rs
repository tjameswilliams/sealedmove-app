//! Server-sent-events plumbing shared by streaming HTTP backends.
//!
//! Both the OpenAI-compat and native Anthropic backends read their streaming
//! responses as SSE over chunked HTTP. The framing concerns are identical —
//! chunks can split lines anywhere (including mid-UTF-8-codepoint), and only
//! `data:` lines carry payloads — so the byte-level handling lives here and
//! each backend supplies its own payload accumulator.

/// Buffers raw response bytes and yields complete lines. SSE payloads can be
/// split across HTTP chunks (including mid-line and even mid-UTF-8-codepoint),
/// so bytes are buffered until a `\n` arrives.
#[derive(Debug, Default)]
pub(crate) struct SseLineBuffer {
    buf: Vec<u8>,
}

impl SseLineBuffer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes; returns all lines completed by it, with the
    /// trailing `\n` (and any `\r`) stripped.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }
}

/// Extract the payload of an SSE `data:` line, or `None` for any other line
/// (comments, `event:` fields, blank keep-alives).
pub(crate) fn sse_data_payload(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("data:")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_buffer_handles_partial_lines_across_chunks() {
        let mut buf = SseLineBuffer::new();
        assert!(buf.feed(b"data: {\"a\"").is_empty());
        let lines = buf.feed(b": 1}\r\ndata: [DO");
        assert_eq!(lines, vec!["data: {\"a\": 1}"]);
        let lines = buf.feed(b"NE]\n\n");
        assert_eq!(lines, vec!["data: [DONE]", ""]);
    }

    #[test]
    fn sse_data_payload_extraction() {
        assert_eq!(sse_data_payload("data: {\"x\":1}"), Some("{\"x\":1}"));
        assert_eq!(sse_data_payload("data:{\"x\":1}"), Some("{\"x\":1}"));
        assert_eq!(sse_data_payload("data: [DONE]"), Some("[DONE]"));
        assert_eq!(sse_data_payload("event: ping"), None);
        assert_eq!(sse_data_payload(": comment"), None);
        assert_eq!(sse_data_payload(""), None);
    }
}
