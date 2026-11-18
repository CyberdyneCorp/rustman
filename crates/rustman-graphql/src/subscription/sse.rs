//! Subscriptions over Server-Sent Events (GraphQL over SSE, distinct
//! connections mode): the operation is POSTed with `Accept:
//! text/event-stream` through `rustman-http` (TLS, proxies, auth, cookies),
//! and every `next` event is recorded until `complete`, the limits or the
//! caller's stop, which closes the connection.
//!
//! The event stream is parsed by a small [`SseParser`] rather than the
//! `eventsource-stream` crate named in design D4: the subset GraphQL over
//! SSE needs (`event`, `data`, comments) fits in a few lines and keeps
//! `nom` out of the dependency tree.

use std::time::{Duration, Instant};

use rustman_core::vars::{Resolved, ResolvedGraphql, ResolvedPair, ResolvedRequest};
use rustman_http::{HttpEngine, SendOptions, StreamedResponse};
use serde_json::Value;

use super::{Collector, Ended, SubscribeOptions, Subscription, SubscriptionProtocol};
use crate::error::GraphqlError;
use crate::execute::HttpOperation;

/// The media type of an event stream.
const EVENT_STREAM: &str = "text/event-stream";

/// One dispatched event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `event` field; `message` when absent.
    pub event: String,
    /// The `data` lines joined with `\n`.
    pub data: String,
}

/// An event (with the line still being read) larger than the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooLarge {
    pub limit: u64,
}

/// An incremental `text/event-stream` parser: feed it chunks as they
/// arrive, in any split, and get the events they complete. Each byte is
/// scanned once, and an event may not grow past `limit` bytes.
#[derive(Debug)]
pub(crate) struct SseParser {
    /// Bytes of a line not yet ended.
    pending: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
    limit: u64,
}

impl SseParser {
    pub fn new(limit: u64) -> Self {
        Self {
            pending: Vec::new(),
            event: None,
            data: None,
            limit,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, TooLarge> {
        let mut events = Vec::new();
        let mut rest = chunk;
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            self.pending.extend_from_slice(&rest[..end]);
            rest = &rest[end + 1..];
            let mut line = std::mem::take(&mut self.pending);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            events.extend(self.line(text(line)));
            self.check()?;
        }
        self.pending.extend_from_slice(rest);
        self.check()?;
        Ok(events)
    }

    /// The bytes held for the event being read must stay within the limit.
    fn check(&self) -> Result<(), TooLarge> {
        let held = self.pending.len() + self.data.as_ref().map_or(0, String::len);
        if held as u64 > self.limit {
            return Err(TooLarge { limit: self.limit });
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
            "event" => self.event = Some(line[value..].to_owned()),
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
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        let data = self.data.take();
        if event.is_none() && data.is_none() {
            return None;
        }
        Some(SseEvent {
            event: event.unwrap_or_else(|| "message".to_owned()),
            data: data.unwrap_or_default(),
        })
    }
}

/// `line` as text; invalid UTF-8 is replaced, and only then copied.
fn text(line: Vec<u8>) -> String {
    String::from_utf8(line).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Run `operation` as a subscription over SSE to the endpoint of `target`.
pub async fn subscribe_sse(
    engine: &HttpEngine,
    send_options: &SendOptions,
    target: &ResolvedGraphql,
    operation: &HttpOperation,
    options: &SubscribeOptions,
) -> Result<Subscription, GraphqlError> {
    let started = Instant::now();
    let request = stream_request(operation, target);
    let limit = options.within(started, send_options.timeout);
    let mut response = open(engine, &request, send_options, options, limit).await?;
    check_stream(&response)?;
    let connected = started.elapsed();
    let mut collector = Collector::new(options, started);
    let mut warnings = std::mem::take(&mut response.warnings);
    // Events are held in memory, so the in-memory body cap bounds them.
    let mut parser = SseParser::new(send_options.body.max_in_memory);
    let read = Reader {
        parser: &mut parser,
        warnings: &mut warnings,
    };
    let ended = collect(&mut response, &mut collector, read).await?;
    let collected = collector.finish();
    Ok(Subscription {
        protocol: SubscriptionProtocol::Sse,
        request,
        status: response.status,
        headers: response.headers.clone(),
        events: collected.events,
        event_count: collected.event_count,
        ended,
        failure: collected.failure,
        connected,
        elapsed: started.elapsed(),
        insecure: response.insecure,
        warnings,
    })
}

/// Open the stream within `limit` (the HTTP timeout, shortened to what is
/// left of a headless `duration`), unless the caller stops first.
async fn open(
    engine: &HttpEngine,
    request: &ResolvedRequest,
    send_options: &SendOptions,
    options: &SubscribeOptions,
    limit: Duration,
) -> Result<StreamedResponse, GraphqlError> {
    let opening = tokio::time::timeout(limit, engine.open_stream(request, send_options));
    let opened = tokio::select! {
        biased;
        () = options.cancel.cancelled() => {
            return Err(GraphqlError::Subscription("SSE: stopped before the stream opened".into()));
        }
        opened = opening => opened,
    };
    let response = opened.map_err(|_| {
        GraphqlError::Subscription(format!(
            "SSE: the stream did not open within {} ms",
            limit.as_millis()
        ))
    })??;
    Ok(response)
}

/// The POST of the operation, asking for an event stream, uncompressed.
fn stream_request(operation: &HttpOperation, target: &ResolvedGraphql) -> ResolvedRequest {
    let mut request = operation.http_request(target, true);
    request
        .headers
        .retain(|h| !h.name.text.eq_ignore_ascii_case("accept"));
    let header = |name: &str, value: &str| ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    };
    request.headers.insert(0, header("Accept", EVENT_STREAM));
    let encoding = request
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case("accept-encoding"));
    if !encoding {
        request.headers.push(header("Accept-Encoding", "identity"));
    }
    request
}

/// Reject a response that is not a successful event stream.
fn check_stream(response: &StreamedResponse) -> Result<(), GraphqlError> {
    let media = response.header("content-type").unwrap_or_default();
    let stream = media
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case(EVENT_STREAM));
    if (200..300).contains(&response.status) && stream {
        return Ok(());
    }
    Err(GraphqlError::Subscription(format!(
        "SSE: HTTP {} with content type '{media}', expected {EVENT_STREAM}",
        response.status
    )))
}

/// What reads the stream: the parser, and the warnings it adds to.
struct Reader<'a> {
    parser: &'a mut SseParser,
    warnings: &'a mut Vec<String>,
}

/// Read events until `complete`, the end of the stream, an event over the
/// size limit, the limits or the caller's stop.
async fn collect(
    response: &mut StreamedResponse,
    collector: &mut Collector<'_>,
    read: Reader<'_>,
) -> Result<Ended, GraphqlError> {
    loop {
        let chunk = tokio::select! {
            biased;
            () = collector.options.cancel.cancelled() => return Ok(Ended::Stopped),
            () = collector.deadline() => return Ok(Ended::Duration),
            chunk = response.chunk() => chunk,
        };
        let Some(chunk) = chunk.transpose()? else {
            collector.fail("SSE: the stream ended without a complete event".into());
            return Ok(Ended::Closed);
        };
        let events = match read.parser.push(&chunk) {
            Ok(events) => events,
            Err(TooLarge { limit }) => {
                collector.fail(format!(
                    "SSE: an event is larger than the {limit} byte limit (max_body_bytes)"
                ));
                return Ok(Ended::Error);
            }
        };
        for event in events {
            if let Some(ended) = apply(event, collector, read.warnings) {
                return Ok(ended);
            }
        }
    }
}

/// Record a `next` event; `Some` when the subscription ends with it.
fn apply(
    event: SseEvent,
    collector: &mut Collector<'_>,
    warnings: &mut Vec<String>,
) -> Option<Ended> {
    match event.event.as_str() {
        "complete" => Some(Ended::Complete),
        "next" => match serde_json::from_str::<Value>(&event.data) {
            Ok(payload) => collector.push(payload).then_some(Ended::MaxEvents),
            Err(_) => {
                warnings.push("graphql: SSE: a next event that is not JSON was skipped".into());
                None
            }
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    #[test]
    fn events_split_anywhere_are_parsed() {
        let stream = ": open\n\nevent: next\ndata: {\"a\":\ndata: 1}\r\n\r\nevent: complete\ndata:\n\ndata: plain\n\n";
        let whole = SseParser::new(1024).push(stream.as_bytes()).unwrap();
        let expected = vec![
            event("next", "{\"a\":\n1}"),
            event("complete", ""),
            event("message", "plain"),
        ];
        assert_eq!(whole, expected);
        let mut parser = SseParser::new(1024);
        let mut split = Vec::new();
        for byte in stream.as_bytes() {
            split.extend(parser.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(split, expected);
    }

    #[test]
    fn unknown_fields_and_unfinished_events_wait() {
        let mut parser = SseParser::new(1024);
        assert!(
            parser
                .push(b"id: 7\nretry: 10\nevent: next\ndata: x")
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(err, TooLarge { limit: 64 });
    }

    #[test]
    fn data_spread_over_many_lines_cannot_grow_past_the_limit() {
        let mut parser = SseParser::new(64);
        let lines = "data: 0123456789\n".repeat(8);
        assert_eq!(parser.push(lines.as_bytes()), Err(TooLarge { limit: 64 }));
        let mut fits = SseParser::new(64);
        let events = fits.push(b"data: 0123456789\ndata: x\n\n").unwrap();
        assert_eq!(events, vec![event("message", "0123456789\nx")]);
    }

    #[test]
    fn fields_without_a_space_or_value_and_invalid_utf8_are_read() {
        let mut parser = SseParser::new(1024);
        let events = parser.push(b"event:next\ndata\ndata:\xff\n\n").unwrap();
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
}
