//! The `text/event-stream` parser (WHATWG HTML, "Server-sent events",
//! interpreting an event stream), shared by GraphQL over SSE and realtime
//! SSE sessions: one parser in the tree (add-realtime-client design D7).
//!
//! It is incremental: feed it chunks as they arrive, split anywhere, and
//! get the events they complete. Lines end with CRLF, LF or CR, a leading
//! byte order mark is skipped, comments are ignored, `data` lines are
//! joined with `\n`, `id` sets the last event id (kept across events, and
//! taken on when an event is dispatched) and a `retry` of ASCII digits sets
//! the reconnection time. Each byte is scanned once, and an event may not
//! grow past the size limit.
//!
//! An `event` or `id` value longer than [`MAX_FIELD_BYTES`] is ignored, as
//! an `id` holding NUL is: both are copied into every event they apply
//! to, so they stay small however large events may be.

/// The media type of an event stream.
pub const EVENT_STREAM: &str = "text/event-stream";

/// The longest `event` or `id` value kept (4 KiB); a longer one is
/// ignored.
pub const MAX_FIELD_BYTES: usize = 4 * 1024;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One dispatched event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field; `message` when absent.
    pub event: String,
    /// The `data` lines joined with `\n`.
    pub data: String,
    /// The last event id when the event was dispatched; `None` while it is
    /// empty.
    pub id: Option<String>,
}

/// An event (with the line still being read) larger than the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SseTooLarge {
    pub limit: u64,
    /// The bytes held for the event when the limit was passed.
    pub size: u64,
}

/// An incremental `text/event-stream` parser.
#[derive(Debug)]
pub struct SseParser {
    /// Bytes of a line not yet ended.
    pending: Vec<u8>,
    /// Whether the start of the stream (and its BOM) is behind us.
    started: bool,
    /// The previous chunk ended with CR: a leading LF ends no new line.
    after_cr: bool,
    event: Option<String>,
    data: Option<String>,
    /// The `id` of the event being read (the WHATWG "last event ID
    /// buffer"): it becomes the last event id when the event is
    /// dispatched.
    id_buffer: String,
    last_event_id: String,
    retry: Option<u64>,
    limit: u64,
    /// Dispatch an event with a type and no data (see
    /// [`SseParser::keeping_typed_events`]).
    typed_without_data: bool,
}

impl SseParser {
    /// A parser whose events may hold at most `limit` bytes.
    pub fn new(limit: u64) -> Self {
        Self {
            pending: Vec::new(),
            started: false,
            after_cr: false,
            event: None,
            data: None,
            id_buffer: String::new(),
            last_event_id: String::new(),
            retry: None,
            limit,
            typed_without_data: false,
        }
    }

    /// This parser, also dispatching an event that has a type but no
    /// `data` line, with empty data. The WHATWG algorithm drops such an
    /// event; GraphQL over SSE keeps it, so that a `complete` event sent
    /// without `data:` still ends the subscription.
    #[must_use]
    pub fn keeping_typed_events(mut self) -> Self {
        self.typed_without_data = true;
        self
    }

    /// This parser, resuming a stream whose last event id was `id` (the
    /// `Last-Event-ID` a reconnection sent).
    #[must_use]
    pub fn resuming(mut self, id: &str) -> Self {
        id.clone_into(&mut self.id_buffer);
        id.clone_into(&mut self.last_event_id);
        self
    }

    /// The last event id the stream set; empty until one is set (or after
    /// an empty `id`). An `id` counts once its event is dispatched (the
    /// blank line ending it), never while the event is still being read.
    /// Sent as `Last-Event-ID` when reconnecting.
    pub fn last_event_id(&self) -> &str {
        &self.last_event_id
    }

    /// The last valid `retry` value, in milliseconds.
    pub fn retry(&self) -> Option<u64> {
        self.retry
    }

    /// Parse `chunk`, returning the events it completes.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseTooLarge> {
        if !self.started {
            return self.push_start(chunk);
        }
        let mut events = Vec::new();
        let mut rest = chunk;
        if self.after_cr {
            self.after_cr = false;
            rest = rest.strip_prefix(b"\n").unwrap_or(rest);
        }
        while let Some(end) = rest.iter().position(|b| *b == b'\n' || *b == b'\r') {
            self.pending.extend_from_slice(&rest[..end]);
            // Checked before the line is applied, so that a line over the
            // limit fails however the stream is split into chunks.
            self.check()?;
            let crlf = rest[end] == b'\r';
            rest = &rest[end + 1..];
            if crlf {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    Some(_) => {}
                    None => self.after_cr = true,
                }
            }
            let line = std::mem::take(&mut self.pending);
            events.extend(self.line(text(line)));
            self.check()?;
        }
        self.pending.extend_from_slice(rest);
        self.check()?;
        Ok(events)
    }

    /// The first bytes of the stream: hold them until it is known whether
    /// they start with a byte order mark, and skip it.
    fn push_start(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseTooLarge> {
        let mut head = std::mem::take(&mut self.pending);
        head.extend_from_slice(chunk);
        if head.len() < BOM.len() && BOM.starts_with(&head) {
            self.pending = head;
            return Ok(Vec::new());
        }
        self.started = true;
        let body = head.strip_prefix(BOM).unwrap_or(&head);
        self.push(body)
    }

    /// The bytes held for the event being read must stay within the limit.
    fn check(&self) -> Result<(), SseTooLarge> {
        let held = (self.pending.len() + self.data.as_ref().map_or(0, String::len)) as u64;
        if held > self.limit {
            return Err(SseTooLarge {
                limit: self.limit,
                size: held,
            });
        }
        Ok(())
    }

    /// Apply one line; an empty line dispatches the event. The first
    /// `data` line becomes the event's data in place, without a copy.
    fn line(&mut self, mut line: String) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(colon) => (
                colon,
                colon + 1 + usize::from(line[colon + 1..].starts_with(' ')),
            ),
            None => (line.len(), line.len()),
        };
        match &line[..field] {
            "event" if line.len() - value <= MAX_FIELD_BYTES => {
                self.event = Some(line[value..].to_owned());
            }
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(&line[value..]);
                }
                None => {
                    line.drain(..value);
                    self.data = Some(line);
                }
            },
            "id" if line.len() - value <= MAX_FIELD_BYTES && !line[value..].contains('\0') => {
                line[value..].clone_into(&mut self.id_buffer);
            }
            "retry" => self.set_retry(&line[value..]),
            _ => {}
        }
        None
    }

    fn set_retry(&mut self, value: &str) {
        if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
            self.retry = Some(value.parse().unwrap_or(u64::MAX));
        }
    }

    /// An event when data was given (or, when kept, a type); the event
    /// type is reset and the last event id set either way.
    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.last_event_id != self.id_buffer {
            self.last_event_id.clone_from(&self.id_buffer);
        }
        let event = self.event.take();
        let typed = event.as_ref().is_some_and(|e| !e.is_empty());
        let data = match self.data.take() {
            Some(data) => data,
            None if typed && self.typed_without_data => String::new(),
            None => return None,
        };
        Some(SseEvent {
            event: event
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "message".to_owned()),
            data,
            id: (!self.last_event_id.is_empty()).then(|| self.last_event_id.clone()),
        })
    }
}

/// `line` as text; invalid UTF-8 is replaced, and only then copied.
fn text(line: Vec<u8>) -> String {
    String::from_utf8(line).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Whether the `Content-Type` value `media` is an event stream.
pub fn is_event_stream(media: &str) -> bool {
    media
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case(EVENT_STREAM))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: None,
        }
    }

    fn with_id(mut e: SseEvent, id: &str) -> SseEvent {
        e.id = Some(id.into());
        e
    }

    fn parse(stream: &[u8]) -> Vec<SseEvent> {
        SseParser::new(1024).push(stream).unwrap()
    }

    /// `stream` fed one byte at a time.
    fn parse_bytewise(stream: &[u8]) -> Vec<SseEvent> {
        let mut parser = SseParser::new(1024);
        let mut events = Vec::new();
        for byte in stream {
            events.extend(parser.push(std::slice::from_ref(byte)).unwrap());
        }
        events
    }

    type Parsed = Option<(Vec<SseEvent>, String, Option<u64>)>;

    /// What `stream` parses to when fed in pieces of `step` bytes; `None`
    /// once the size limit is passed (the property of the fuzz target).
    fn parse_in_steps(stream: &[u8], step: usize, limit: u64) -> Parsed {
        let mut parser = SseParser::new(limit);
        let mut events = Vec::new();
        for chunk in stream.chunks(step) {
            events.extend(parser.push(chunk).ok()?);
        }
        Some((events, parser.last_event_id().to_owned(), parser.retry()))
    }

    /// A stream of SSE-shaped pieces chosen by a fixed xorshift sequence.
    fn generated_stream(seed: &mut u64) -> Vec<u8> {
        const PIECES: [&[u8]; 14] = [
            b"data: ",
            b"event: ",
            b"id: ",
            b"retry: ",
            b": c",
            b"\n",
            b"\r",
            b"\r\n",
            b"\xEF\xBB\xBF",
            b"12",
            b"x",
            b"\xFF",
            b"\0",
            b":",
        ];
        let mut stream = Vec::new();
        for _ in 0..(next(seed) % 40) {
            stream.extend_from_slice(PIECES[(next(seed) % 14) as usize]);
        }
        stream
    }

    fn next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn how_a_generated_stream_is_split_never_changes_what_it_parses_to() {
        let mut seed = 0x2545_F491_4F6C_DD1D;
        for _ in 0..5_000 {
            let stream = generated_stream(&mut seed);
            let step = (next(&mut seed) % 16 + 1) as usize;
            let whole = parse_in_steps(&stream, stream.len().max(1), 64);
            assert_eq!(
                whole,
                parse_in_steps(&stream, step, 64),
                "{stream:?} by {step}"
            );
        }
    }

    #[test]
    fn events_split_anywhere_are_parsed() {
        let stream = ": open\n\nevent: next\ndata: {\"a\":\ndata: 1}\r\n\r\nevent: complete\ndata:\n\ndata: plain\n\n";
        let expected = vec![
            event("next", "{\"a\":\n1}"),
            event("complete", ""),
            event("message", "plain"),
        ];
        assert_eq!(parse(stream.as_bytes()), expected);
        assert_eq!(parse_bytewise(stream.as_bytes()), expected);
    }

    #[test]
    fn multi_line_data_with_type_and_id() {
        let stream = b"event: update\ndata: line1\ndata: line2\nid: 7\n\n";
        let expected = vec![with_id(event("update", "line1\nline2"), "7")];
        assert_eq!(parse(stream), expected);
    }

    #[test]
    fn crlf_cr_and_lf_line_endings_are_equivalent() {
        let lf = b"data: a\ndata: b\n\ndata: c\n\n".as_slice();
        let crlf = b"data: a\r\ndata: b\r\n\r\ndata: c\r\n\r\n".as_slice();
        let cr = b"data: a\rdata: b\r\rdata: c\r\r".as_slice();
        let expected = vec![event("message", "a\nb"), event("message", "c")];
        for stream in [lf, crlf, cr] {
            assert_eq!(parse(stream), expected);
            assert_eq!(parse_bytewise(stream), expected);
        }
    }

    #[test]
    fn a_crlf_split_between_chunks_ends_one_line() {
        let mut parser = SseParser::new(1024);
        assert!(parser.push(b"data: a\r").unwrap().is_empty());
        assert!(parser.push(b"\n").unwrap().is_empty());
        assert_eq!(parser.push(b"\r\n").unwrap(), vec![event("message", "a")]);
    }

    #[test]
    fn comments_are_ignored() {
        let stream = b": keep-alive\ndata: x\n: between\n\n:\n\n";
        assert_eq!(parse(stream), vec![event("message", "x")]);
    }

    #[test]
    fn the_last_event_id_carries_over_and_can_be_cleared() {
        let mut parser = SseParser::new(1024);
        let events = parser
            .push(b"id: 41\ndata: a\n\ndata: b\n\nid\ndata: c\n\n")
            .unwrap();
        assert_eq!(
            events,
            vec![
                with_id(event("message", "a"), "41"),
                with_id(event("message", "b"), "41"),
                event("message", "c"),
            ]
        );
        assert_eq!(parser.last_event_id(), "");
        parser.push(b"id: 42\n\n").unwrap();
        assert_eq!(parser.last_event_id(), "42");
        parser.push(b"id: 4\x003\n\n").unwrap();
        assert_eq!(parser.last_event_id(), "42", "an id with NUL is ignored");
    }

    #[test]
    fn the_last_event_id_changes_only_when_the_event_is_dispatched() {
        let mut parser = SseParser::new(1024).resuming("1");
        assert!(parser.push(b"id: 2\ndata: a\n").unwrap().is_empty());
        assert_eq!(parser.last_event_id(), "1", "the event is not over yet");
        let events = parser.push(b"\n").unwrap();
        assert_eq!(events, vec![with_id(event("message", "a"), "2")]);
        assert_eq!(parser.last_event_id(), "2");
        parser.push(b"id: 3\ndata: cut").unwrap();
        assert_eq!(
            parser.last_event_id(),
            "2",
            "an event the stream never ended does not count"
        );
    }

    #[test]
    fn event_and_id_values_over_the_field_limit_are_ignored() {
        let long = "x".repeat(MAX_FIELD_BYTES + 1);
        let fits = "y".repeat(MAX_FIELD_BYTES);
        let mut parser = SseParser::new(1024 * 1024);
        let stream = format!(
            "id: 7\nevent: {long}\nid: {long}\ndata: a\n\nevent: {fits}\nid: {fits}\ndata: b\n\n"
        );
        let events = parser.push(stream.as_bytes()).unwrap();
        assert_eq!(
            events,
            vec![
                with_id(event("message", "a"), "7"),
                with_id(event(&fits, "b"), &fits),
            ]
        );
        assert_eq!(parser.last_event_id(), fits);
    }

    #[test]
    fn a_resumed_stream_keeps_its_last_event_id() {
        let mut parser = SseParser::new(1024).resuming("42");
        let events = parser.push(b"data: a\n\n").unwrap();
        assert_eq!(events, vec![with_id(event("message", "a"), "42")]);
        assert_eq!(parser.last_event_id(), "42");
    }

    #[test]
    fn retry_takes_only_ascii_digits() {
        let mut parser = SseParser::new(1024);
        assert_eq!(parser.retry(), None);
        parser.push(b"retry: 1500\n\n").unwrap();
        assert_eq!(parser.retry(), Some(1500));
        parser.push(b"retry: 2s\nretry: -1\nretry:\n\n").unwrap();
        assert_eq!(parser.retry(), Some(1500));
        parser.push(b"retry:20\n").unwrap();
        assert_eq!(parser.retry(), Some(20));
    }

    #[test]
    fn events_without_data_are_not_dispatched() {
        assert!(parse(b"event: ping\n\nid: 3\n\nretry: 9\n\n").is_empty());
        assert_eq!(parse(b"event:\ndata: x\n\n"), vec![event("message", "x")]);
    }

    #[test]
    fn typed_events_without_data_are_kept_when_asked() {
        let mut parser = SseParser::new(1024).keeping_typed_events();
        let events = parser
            .push(b"event: complete\n\nid: 3\n\nevent:\n\n")
            .unwrap();
        assert_eq!(events, vec![event("complete", "")]);
    }

    #[test]
    fn a_line_over_the_limit_fails_however_it_is_split() {
        for line in [": comment", "foo: unknown", "event: name", "data: value"] {
            let stream = format!("{line}{}\n\ndata: ok\n\n", "x".repeat(80));
            let whole = SseParser::new(64).push(stream.as_bytes());
            assert!(whole.is_err(), "{line}: in one chunk");
            let mut parser = SseParser::new(64);
            let split = stream
                .as_bytes()
                .chunks(7)
                .try_for_each(|chunk| parser.push(chunk).map(drop));
            assert!(split.is_err(), "{line}: in pieces");
        }
    }

    #[test]
    fn a_leading_byte_order_mark_is_skipped_once() {
        let stream = b"\xEF\xBB\xBFdata: a\n\n";
        assert_eq!(parse(stream), vec![event("message", "a")]);
        assert_eq!(parse_bytewise(stream), vec![event("message", "a")]);
        let twice = b"\xEF\xBB\xBF\xEF\xBB\xBFdata: a\n\n";
        assert!(
            parse(twice).is_empty(),
            "a second BOM is part of the field name"
        );
        assert_eq!(parse(b"\xEFdata: a\n\n").len(), 0);
        assert_eq!(parse(b"da"), vec![]);
    }

    #[test]
    fn unknown_fields_and_unfinished_events_wait() {
        let mut parser = SseParser::new(1024);
        let pushed = parser.push(b"foo: 7\nevent: next\ndata: x").unwrap();
        assert!(pushed.is_empty());
        assert_eq!(parser.push(b"\n\n").unwrap(), vec![event("next", "x")]);
    }

    #[test]
    fn a_line_without_end_cannot_grow_past_the_limit() {
        let mut parser = SseParser::new(64);
        assert!(parser.push(b"data: ").unwrap().is_empty());
        let chunk = [b'x'; 16];
        let mut pushed = 0;
        let err = loop {
            match parser.push(&chunk) {
                Ok(events) => assert!(events.is_empty()),
                Err(err) => break err,
            }
            pushed += 1;
            assert!(pushed < 10, "the limit was never reached");
        };
        assert_eq!(err.limit, 64);
        assert!(err.size > 64);
    }

    #[test]
    fn data_spread_over_many_lines_cannot_grow_past_the_limit() {
        let mut parser = SseParser::new(64);
        let lines = "data: 0123456789\n".repeat(8);
        assert_eq!(parser.push(lines.as_bytes()).unwrap_err().limit, 64);
        let mut fits = SseParser::new(64);
        let events = fits.push(b"data: 0123456789\ndata: x\n\n").unwrap();
        assert_eq!(events, vec![event("message", "0123456789\nx")]);
    }

    #[test]
    fn fields_without_a_space_or_value_and_invalid_utf8_are_read() {
        let events = parse(b"event:next\ndata\ndata:\xff\n\n");
        assert_eq!(events, vec![event("next", "\n\u{fffd}")]);
    }

    #[test]
    fn a_long_line_in_small_chunks_is_scanned_once() {
        // 4 MiB in 64-byte chunks: rescanning the pending line on every
        // push would compare bytes about 10^11 times.
        let mut parser = SseParser::new(8 * 1024 * 1024);
        parser.push(b"event: next\ndata: ").unwrap();
        let chunk = [b'a'; 64];
        for _ in 0..(4 * 1024 * 1024 / 64) {
            assert!(parser.push(&chunk).unwrap().is_empty());
        }
        let events = parser.push(b"\n\n").unwrap();
        assert_eq!(events[0].data.len(), 4 * 1024 * 1024);
    }

    #[test]
    fn event_stream_media_types_are_recognized() {
        assert!(is_event_stream("text/event-stream"));
        assert!(is_event_stream("Text/Event-Stream; charset=utf-8"));
        assert!(!is_event_stream("application/json"));
        assert!(!is_event_stream(""));
    }
}
