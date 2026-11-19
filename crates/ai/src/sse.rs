// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Incremental server-sent-events parser. Lines are split on bytes, so a multi-byte
//! character cut by a network chunk boundary survives.

/// Longer than any honest event; past it a server is only filling our memory.
pub(crate) const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    skip_lf: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    overflow: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once a line or an event passed `MAX_EVENT_BYTES`: the stream is unusable and
    /// the parser ignores the rest of it. The caller turns this into an error.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    fn give_up(&mut self) {
        self.overflow = true;
        self.line = Vec::new();
        self.data = String::new();
        self.has_data = false;
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.overflow {
                break;
            }
            if self.skip_lf {
                self.skip_lf = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' => self.end_line(&mut out),
                b'\r' => {
                    self.end_line(&mut out);
                    self.skip_lf = true;
                }
                _ if self.line.len() >= MAX_EVENT_BYTES => self.give_up(),
                _ => self.line.push(b),
            }
        }
        out
    }

    /// End of stream: a final event without its blank line is still delivered.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if self.overflow {
            return out;
        }
        if !self.line.is_empty() {
            self.end_line(&mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn end_line(&mut self, out: &mut Vec<SseEvent>) {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        let text = String::from_utf8_lossy(&line);
        if text.starts_with(':') {
            return;
        }
        let (field, value) = match text.find(':') {
            Some(i) => {
                let v = &text[i + 1..];
                (&text[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (&text[..], ""),
        };
        match field {
            "data" => {
                if self.data.len() + value.len() >= MAX_EVENT_BYTES {
                    self.give_up();
                    return;
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = Some(value.to_string()),
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.has_data {
            out.push(SseEvent {
                event: self.event.take(),
                data: std::mem::take(&mut self.data),
            });
        }
        self.event = None;
        self.has_data = false;
    }
}

/// Test support: cuts a byte stream at pseudo-random places.
#[cfg(test)]
pub(crate) fn random_cuts(bytes: &[u8], seed: u64) -> Vec<&[u8]> {
    let mut x = seed.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = 1 + (x % 23) as usize;
        let end = (at + n).min(bytes.len());
        out.push(&bytes[at..end]);
        at = end;
    }
    out
}

/// Test support: merges adjacent text and argument fragments, so that streams cut
/// differently compare equal.
#[cfg(test)]
pub(crate) fn collapse(
    events: Vec<crate::provider::StreamEvent>,
) -> Vec<crate::provider::StreamEvent> {
    use crate::provider::StreamEvent as E;
    let mut out: Vec<E> = Vec::new();
    for e in events {
        if let (Some(E::TextDelta(a)), E::TextDelta(b)) = (out.last_mut(), &e) {
            a.push_str(b);
            continue;
        }
        if let (Some(E::ToolArgsDelta { id: prev, json: a }), E::ToolArgsDelta { id, json }) =
            (out.last_mut(), &e)
        {
            if *prev == *id {
                a.push_str(json);
                continue;
            }
        }
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.feed(c));
        }
        out.extend(p.finish());
        out
    }

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(str::to_string),
            data: data.to_string(),
        }
    }

    #[test]
    fn event_field_comment_and_multiline_data() {
        let got = parse(&[b": ping\n\nevent: a\ndata: one\ndata: two\n\ndata:three\n\n"]);
        assert_eq!(got, vec![ev(Some("a"), "one\ntwo"), ev(None, "three")]);
    }

    #[test]
    fn all_line_endings_agree_even_when_cr_lf_is_split() {
        let want = vec![ev(None, "x"), ev(None, "y")];
        assert_eq!(parse(&[b"data: x\r\n\r\ndata: y\r\n\r\n"]), want);
        assert_eq!(parse(&[b"data: x\r", b"\n\r", b"\ndata: y\r\n\r\n"]), want);
        assert_eq!(parse(&[b"data: x\r\rdata: y\r\r"]), want);
    }

    #[test]
    fn utf8_split_inside_a_character_is_intact() {
        let s = "data: \u{042d}\u{0442}\u{0430}\u{0436} 3 \u{2014} \u{0434}\u{0432}\u{0435}\u{0440}\u{0438}\n\n".as_bytes();
        for cut in 1..s.len() {
            let (a, b) = s.split_at(cut);
            assert_eq!(
                parse(&[a, b]),
                vec![ev(None, "\u{042d}\u{0442}\u{0430}\u{0436} 3 \u{2014} \u{0434}\u{0432}\u{0435}\u{0440}\u{0438}")],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn final_event_without_blank_line_is_delivered() {
        assert_eq!(parse(&[b"data: tail"]), vec![ev(None, "tail")]);
    }

    #[test]
    fn empty_data_is_an_event_but_a_lone_blank_line_is_not() {
        assert_eq!(parse(&[b"\n\ndata:\n\n"]), vec![ev(None, "")]);
    }

    #[test]
    fn a_line_without_end_cannot_grow_past_the_cap() {
        let mut p = SseParser::new();
        assert_eq!(p.feed(b"data: ok\n\n"), vec![ev(None, "ok")]);
        let piece = vec![b'x'; 1024 * 1024];
        for _ in 0..(MAX_EVENT_BYTES / piece.len() + 2) {
            assert!(p.feed(&piece).is_empty());
        }
        assert!(p.overflowed());
        assert_eq!(p.line.capacity(), 0, "the buffer is released");
        assert!(p.feed(b"\ndata: late\n\n").is_empty());
        assert!(p.finish().is_empty());
    }

    #[test]
    fn many_short_data_lines_cannot_grow_past_the_cap() {
        let mut p = SseParser::new();
        let line = format!("data: {}\n", "y".repeat(1024 * 1024));
        for _ in 0..(MAX_EVENT_BYTES / line.len() + 2) {
            assert!(p.feed(line.as_bytes()).is_empty());
        }
        assert!(p.overflowed());
        assert!(p.data.is_empty());
    }
}
