//! Streaming helpers for the voice pipeline.
//!
//! [`SentenceChunker`] sits between a backend's `complete_stream` deltas and
//! the TTS engine: feed it raw [`super::StreamEvent::TextDelta`] fragments and
//! it emits whole sentences as soon as they complete, so speech synthesis can
//! start before the model finishes the reply.

/// Accumulates streamed text deltas and yields complete sentences.
///
/// Boundary rules:
/// - `.`, `!`, or `?` followed by whitespace ends a sentence. Because a
///   terminator at the very end of the buffer might be continued by the next
///   delta (e.g. `"3."` then `"5"`), end-of-input is only treated as a
///   boundary by [`SentenceChunker::flush`].
/// - A `.` between two digits (a decimal like `3.5`) is never a boundary.
/// - A newline always ends the current sentence.
#[derive(Debug, Default)]
pub struct SentenceChunker {
    buf: String,
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a text delta; returns any sentences completed by it (trimmed,
    /// possibly empty if no boundary was reached yet).
    pub fn feed(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(end) = find_boundary(&self.buf) {
            let sentence: String = self.buf.drain(..end).collect();
            let trimmed = sentence.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        out
    }

    /// Return whatever is left in the buffer as a final sentence, if any.
    pub fn flush(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

/// Find the byte offset just past the first sentence boundary in `text`, or
/// `None` if no complete sentence is present yet.
fn find_boundary(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if c == '\n' {
            return Some(i + c.len_utf8());
        }
        if matches!(c, '.' | '!' | '?') {
            // Need lookahead: a terminator at end-of-buffer may be continued
            // by the next delta, so only whitespace confirms the boundary.
            let Some(&(_, next)) = chars.get(k + 1) else {
                continue;
            };
            if !next.is_whitespace() {
                continue;
            }
            // Decimal guard: "3. 5" would already pass (next is whitespace);
            // the case to protect is digit '.' digit, which fails the
            // whitespace check anyway. Guard explicitly for clarity in case
            // the whitespace rule ever loosens.
            if c == '.' {
                let prev_is_digit = k
                    .checked_sub(1)
                    .and_then(|j| chars.get(j))
                    .map(|&(_, p)| p.is_ascii_digit())
                    .unwrap_or(false);
                let next_is_digit = next.is_ascii_digit();
                if prev_is_digit && next_is_digit {
                    continue;
                }
            }
            return Some(i + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunker: &mut SentenceChunker, deltas: &[&str]) -> Vec<String> {
        deltas.iter().flat_map(|d| chunker.feed(d)).collect()
    }

    #[test]
    fn splits_on_period_followed_by_space() {
        let mut c = SentenceChunker::new();
        let got = c.feed("Nice move. Now develop your knight.");
        assert_eq!(got, vec!["Nice move."]);
        assert_eq!(c.flush(), Some("Now develop your knight.".to_string()));
    }

    #[test]
    fn handles_boundary_split_across_deltas() {
        let mut c = SentenceChunker::new();
        let got = feed_all(&mut c, &["Good", " idea", ". But watch", " the bishop! Also"]);
        assert_eq!(got, vec!["Good idea.", "But watch the bishop!"]);
        assert_eq!(c.flush(), Some("Also".to_string()));
    }

    #[test]
    fn does_not_split_inside_decimal_numbers() {
        let mut c = SentenceChunker::new();
        let got = c.feed("The eval is +3.5 here. Impressive!");
        assert_eq!(got, vec!["The eval is +3.5 here."]);
        // Trailing "Impressive!" awaits more input (could be "Impressive!!").
        assert_eq!(c.flush(), Some("Impressive!".to_string()));
    }

    #[test]
    fn trailing_terminator_waits_for_flush() {
        let mut c = SentenceChunker::new();
        // "3." at end of a delta could be continued by "5" in the next.
        assert!(c.feed("The eval is 3.").is_empty());
        let got = c.feed("5 pawns. Great.");
        assert_eq!(got, vec!["The eval is 3.5 pawns."]);
        assert_eq!(c.flush(), Some("Great.".to_string()));
    }

    #[test]
    fn newline_is_a_boundary() {
        let mut c = SentenceChunker::new();
        let got = c.feed("First line\nSecond line. More");
        assert_eq!(got, vec!["First line", "Second line."]);
        assert_eq!(c.flush(), Some("More".to_string()));
    }

    #[test]
    fn question_and_exclamation_marks() {
        let mut c = SentenceChunker::new();
        let got = c.feed("Why Qh5? That hangs the queen! Try Nf3 instead.");
        assert_eq!(got, vec!["Why Qh5?", "That hangs the queen!"]);
        assert_eq!(c.flush(), Some("Try Nf3 instead.".to_string()));
    }

    #[test]
    fn multiple_terminators_stay_together() {
        let mut c = SentenceChunker::new();
        let got = c.feed("Really?! Wait... okay.");
        assert_eq!(got, vec!["Really?!", "Wait..."]);
        assert_eq!(c.flush(), Some("okay.".to_string()));
    }

    #[test]
    fn flush_on_empty_returns_none() {
        let mut c = SentenceChunker::new();
        assert_eq!(c.flush(), None);
        c.feed("Done. ");
        assert_eq!(c.flush(), None); // everything already emitted...
    }

    #[test]
    fn whitespace_only_remainder_is_dropped() {
        let mut c = SentenceChunker::new();
        let got = c.feed("One sentence.   ");
        assert_eq!(got, vec!["One sentence."]);
        assert_eq!(c.flush(), None);
    }
}
