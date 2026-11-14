//! A server-sent-events reader for the provider crates: bytes in, whole events out.
//!
//! It takes the bytes as the socket delivers them, so a line (or a multi-byte character) may
//! be split across two `push` calls. Bad input is an error, never a panic.

/// One dispatched event: the `event:` name (empty when there was none) and the `data:` lines
/// joined with newlines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    #[error("a line is not valid UTF-8")]
    Utf8,
    #[error("a line is longer than {SSE_MAX_LINE} bytes")]
    LineTooLong,
}

/// The longest line accepted. A model reply streams in small pieces, so this only stops a
/// server that never sends a newline from growing the buffer without end.
const SSE_MAX_LINE: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes and returns the events they completed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        loop {
            let rest = &self.buf[start..];
            let Some(at) = rest.iter().position(|b| matches!(b, b'\n' | b'\r')) else {
                break;
            };
            // A CR at the very end may be the first half of CRLF: wait for the next byte.
            if rest[at] == b'\r' && at + 1 == rest.len() {
                break;
            }
            let skip = if rest[at] == b'\r' && rest[at + 1] == b'\n' {
                2
            } else {
                1
            };
            let line = std::str::from_utf8(&rest[..at])
                .map_err(|_| SseError::Utf8)?
                .to_owned();
            start += at + skip;
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        self.buf.drain(..start);
        if self.buf.len() > SSE_MAX_LINE {
            return Err(SseError::LineTooLong);
        }
        Ok(out)
    }

    /// The stream ended: a last line without its newline still counts, and so does an event
    /// without its closing blank line (some servers stop there).
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseError> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            out.extend(self.push(b"\n")?);
        }
        if !self.data.is_empty() {
            out.extend(self.push(b"\n")?);
        }
        Ok(out)
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let event = std::mem::take(&mut self.event);
            if self.data.is_empty() {
                return None;
            }
            return Some(SseEvent {
                event,
                data: std::mem::take(&mut self.data).join("\n"),
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Result<Vec<SseEvent>, SseError> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c)?);
        }
        out.extend(p.finish()?);
        Ok(out)
    }

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    #[test]
    fn events_split_at_any_byte_come_out_whole() {
        let text =
            "event: a\ndata: one\n\n: comment\ndata: x\ndata: y\n\nevent: b\r\ndata: z\r\n\r\n";
        let whole = all(&[text.as_bytes()]).unwrap();
        assert_eq!(whole, [ev("a", "one"), ev("", "x\ny"), ev("b", "z")]);
        for cut in 0..text.len() {
            let (a, b) = text.as_bytes().split_at(cut);
            assert_eq!(all(&[a, b]).unwrap(), whole, "split at {cut}");
        }
    }

    #[test]
    fn a_multibyte_character_may_straddle_two_chunks() {
        let text = "data: h\u{e9}llo \u{1F600}\n\n".as_bytes();
        for cut in 0..text.len() {
            let (a, b) = text.split_at(cut);
            assert_eq!(all(&[a, b]).unwrap(), [ev("", "h\u{e9}llo \u{1F600}")]);
        }
    }

    #[test]
    fn a_lone_cr_ends_a_line() {
        assert_eq!(all(&[b"data: a\r\r"]).unwrap(), [ev("", "a")]);
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        assert_eq!(all(&[b"data: \xff\xfe\n\n"]), Err(SseError::Utf8));
    }

    #[test]
    fn a_final_event_without_its_blank_line_is_still_delivered_by_finish() {
        assert_eq!(all(&[b"data: tail"]).unwrap(), [ev("", "tail")]);
        assert_eq!(all(&[b"data: tail\n"]).unwrap(), [ev("", "tail")]);
    }

    #[test]
    fn a_line_with_no_end_is_cut_off_at_the_cap() {
        let mut p = SseParser::new();
        let chunk = vec![b'a'; 1024 * 1024];
        let mut r = Ok(Vec::new());
        for _ in 0..9 {
            r = p.push(&chunk);
            if r.is_err() {
                break;
            }
        }
        assert_eq!(r, Err(SseError::LineTooLong));
    }

    #[test]
    fn unknown_fields_and_empty_events_are_ignored() {
        assert_eq!(
            all(&[b"id: 7\nretry: 5\n\nevent: only\n\ndata\n\n"]).unwrap(),
            [ev("", "")]
        );
    }
}
