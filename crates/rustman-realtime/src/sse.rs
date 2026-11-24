//! Server-Sent Events sessions (realtime-sessions "SSE request file" to
//! "SSE reconnect and resume"): the stream opens through
//! `HttpEngine::open_stream` (auth, signing, cookies, TLS and proxies as
//! for any HTTP request, GET or POST), events are parsed by
//! `rustman-http`'s WHATWG [`SseParser`], and a dropped stream is resumed
//! after the last `retry` with `Last-Event-ID`, up to
//! `reconnect.max_attempts`. A `204 No Content` ends the session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustman_core::Redactor;
use rustman_core::model::MAX_RETRY_MS;
use rustman_core::vars::{Resolved, ResolvedPair, ResolvedRequest, ResolvedSse};
use rustman_http::{
    HttpEngine, SendOptions, SseEvent, SseParser, SseTooLarge, StreamedResponse, header_pairs,
    is_event_stream,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::RealtimeError;
use crate::exchange::{Handshake, RealtimeExchange, SessionEnd};
use crate::session::{Bounds, Command, SessionOptions, forward, recv, until};
use crate::timeline::{Direction, TimelineEntry};

/// Run the SSE session `target` through `engine` with `options` until its
/// bounds, the server or a cancel end it. The exchange always comes back;
/// its `error` says why the session failed.
pub async fn run_sse(
    engine: &HttpEngine,
    target: &ResolvedSse,
    options: &SendOptions,
    session: SessionOptions,
) -> RealtimeExchange {
    let started = Instant::now();
    let max_timeline = target.limits.effective_max_timeline();
    let exchange = RealtimeExchange::new("sse", target.http.clone(), started, max_timeline);
    let mut stream = Stream::new(target, exchange, session, started);
    let result = stream.run(engine, options).await;
    stream.finish(result, started)
}

/// What happened to the open stream.
enum Flow {
    /// The session is over; its end is recorded.
    Done,
    /// The stream ended or broke: reconnect, if attempts are left.
    Dropped(&'static str),
}

/// What woke the reader.
enum Wake {
    Chunk(Option<Result<bytes::Bytes, rustman_http::RequestError>>),
    Stop(SessionEnd),
    Command(Command),
}

/// A session in progress.
struct Stream<'t> {
    target: &'t ResolvedSse,
    exchange: RealtimeExchange,
    redactor: Redactor,
    bounds: Option<Bounds>,
    deadline: Option<Instant>,
    cancel: CancellationToken,
    feed: Option<mpsc::UnboundedSender<TimelineEntry>>,
    commands: Option<mpsc::Receiver<Command>>,
    last_event_id: Option<Arc<str>>,
    retry: Duration,
    /// The last `retry` the server sent, as sent.
    server_retry: Option<u64>,
    /// Reconnections made so far.
    attempts: u32,
}

impl<'t> Stream<'t> {
    fn new(
        target: &'t ResolvedSse,
        exchange: RealtimeExchange,
        session: SessionOptions,
        started: Instant,
    ) -> Self {
        let retry = Duration::from_millis(rustman_core::model::DEFAULT_RETRY_MS);
        Self {
            target,
            exchange,
            redactor: Redactor::for_request(&target.http),
            bounds: session.bounds,
            deadline: session.bounds.map(|b| started + b.duration),
            cancel: session.cancel,
            feed: session.feed,
            commands: session.commands,
            last_event_id: target
                .last_event_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(Arc::from),
            retry,
            server_retry: None,
            attempts: 0,
        }
    }

    async fn run(
        &mut self,
        engine: &HttpEngine,
        options: &SendOptions,
    ) -> Result<(), RealtimeError> {
        loop {
            let opened = match self.open(engine, options).await {
                Ok(opened) => opened,
                Err(error) if self.may_retry() => {
                    self.note(&format!("reconnect failed: {error}"));
                    if self.wait_retry().await {
                        continue;
                    }
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let Some(response) = opened else {
                return Ok(());
            };
            match self.read(response).await? {
                Flow::Done => return Ok(()),
                Flow::Dropped(why) => {
                    self.record(Direction::System, "close", 0, why.to_owned());
                    if !self.reconnect_after(why).await {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Whether a failed reconnection may be tried again.
    fn may_retry(&self) -> bool {
        self.attempts > 0 && self.attempts < self.target.reconnect.effective_max_attempts()
    }

    /// Open the stream; `None` when the session ended before it opened
    /// (cancelled, out of time, or `204 No Content`).
    async fn open(
        &mut self,
        engine: &HttpEngine,
        options: &SendOptions,
    ) -> Result<Option<StreamedResponse>, RealtimeError> {
        let request = self.request();
        let opening = engine.open_stream(&request, options);
        let response = tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(SessionEnd::new("cancelled")),
            () = until(self.deadline) => Err(self.duration_end()),
            opened = opening => Ok(opened.map_err(RealtimeError::SseTransport)?),
        };
        let response = match response {
            Ok(response) => response,
            Err(end) => {
                self.stop(end);
                return Ok(None);
            }
        };
        self.exchange.credentials.merge(&response.credentials);
        self.redactor.merge(&response.credentials);
        if self.exchange.handshake.is_none() {
            self.exchange.request = request;
            self.exchange.handshake = Some(handshake_of(&response));
        }
        if response.status == 204 {
            let note = "server asked to stop: 204 No Content";
            self.record(Direction::System, "close", 0, note.to_owned());
            self.stop(SessionEnd::new(note));
            return Ok(None);
        }
        check(&response)?;
        self.connected(response.status);
        Ok(Some(response))
    }

    /// The request to send: the file's, with `Last-Event-ID` when there is
    /// a last event id, and asking for an uncompressed stream.
    fn request(&self) -> ResolvedRequest {
        let mut request = self.target.http.clone();
        let header = |name: &str, value: &str| ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        };
        if let Some(id) = &self.last_event_id {
            request
                .headers
                .retain(|h| !h.name.text.eq_ignore_ascii_case("last-event-id"));
            request.headers.push(header("Last-Event-ID", id));
        }
        if !has_header(&request, "accept-encoding") {
            request.headers.push(header("Accept-Encoding", "identity"));
        }
        request
    }

    fn connected(&mut self, status: u16) {
        let (kind, note) = match self.attempts {
            0 => ("connect", format!("connected (HTTP {status})")),
            n => {
                let id = self.last_event_id.as_deref().unwrap_or("none");
                let note = format!("reconnected (attempt {n}, Last-Event-ID: {id})");
                ("reconnect", note)
            }
        };
        self.record(Direction::System, kind, 0, note);
    }

    /// Read events until the session ends or the stream drops.
    async fn read(&mut self, mut response: StreamedResponse) -> Result<Flow, RealtimeError> {
        let limit = self.target.limits.effective_max_message_bytes();
        let resumed = self.last_event_id.as_deref().unwrap_or_default();
        let mut parser = SseParser::new(limit).resuming(resumed);
        loop {
            let chunk = match self.wake(&mut response).await {
                Wake::Chunk(chunk) => chunk,
                Wake::Stop(end) => {
                    self.stop(end);
                    return Ok(Flow::Done);
                }
                Wake::Command(command) => {
                    if self.command(command) {
                        return Ok(Flow::Done);
                    }
                    continue;
                }
            };
            let bytes = match chunk {
                None => return Ok(Flow::Dropped("stream ended")),
                Some(Err(_)) => return Ok(Flow::Dropped("connection dropped")),
                Some(Ok(bytes)) => bytes,
            };
            let events = parser.push(&bytes).map_err(|e| self.too_large(e))?;
            self.resume_state(&parser);
            if self.received_all(events) {
                return Ok(Flow::Done);
            }
        }
    }

    /// The next chunk, unless the session stops or a command comes first.
    async fn wake(&mut self, response: &mut StreamedResponse) -> Wake {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Wake::Stop(SessionEnd::new("cancelled")),
            () = until(self.deadline) => Wake::Stop(self.duration_end()),
            Some(command) = recv(&mut self.commands) => Wake::Command(command),
            chunk = response.chunk() => Wake::Chunk(chunk),
        }
    }

    /// Perform `command`; `true` when it ends the session. An SSE session
    /// sends nothing: frames are noted as errors.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Send(_) => {
                let note = "sse: a stream cannot send frames".to_owned();
                self.record(Direction::System, "error", 0, note);
                false
            }
            Command::Close(_) => {
                self.stop(SessionEnd::new("closed by client"));
                true
            }
        }
    }

    /// Keep the last event id and `retry` the parser saw. A `retry` over
    /// [`MAX_RETRY_MS`] is capped, with a note.
    fn resume_state(&mut self, parser: &SseParser) {
        self.last_event_id = parser.shared_last_event_id();
        let Some(ms) = parser.retry().filter(|&ms| self.server_retry != Some(ms)) else {
            return;
        };
        self.server_retry = Some(ms);
        self.retry = Duration::from_millis(ms.min(MAX_RETRY_MS));
        if ms > MAX_RETRY_MS {
            self.note(&format!("retry of {ms} ms capped at {MAX_RETRY_MS} ms"));
        }
    }

    /// Record `events`; `true` once `max_messages` were received.
    fn received_all(&mut self, events: Vec<SseEvent>) -> bool {
        for event in events {
            self.exchange.received_one();
            let size = event.data.len() as u64;
            let entry = TimelineEntry {
                dir: Direction::Received,
                t_ms: self.exchange.timeline.now_ms(),
                size,
                kind: event.event,
                payload: event.data,
                id: event.id,
                rtt_ms: None,
            };
            self.push(entry);
            if let Some(b) = self.bounds
                && self.exchange.received >= b.max_messages
            {
                let end = format!("max_messages reached ({})", b.max_messages);
                self.stop(SessionEnd::new(end));
                return true;
            }
        }
        false
    }

    /// An event over the limit aborts the stream.
    fn too_large(&mut self, error: SseTooLarge) -> RealtimeError {
        let note = format!("message too large: {} bytes", error.size);
        self.record(Direction::System, "close", 0, note.clone());
        self.stop(SessionEnd::new(note));
        RealtimeError::SseTooLarge(error.size)
    }

    /// After the stream dropped with `why`: wait `retry` and go on, unless
    /// the attempts are used up or the session stops meanwhile.
    async fn reconnect_after(&mut self, why: &str) -> bool {
        if self.attempts >= self.target.reconnect.effective_max_attempts() {
            self.stop(SessionEnd::new(why));
            return false;
        }
        self.wait_retry().await
    }

    /// Wait `retry` before the next attempt; `false` when the session
    /// stopped meanwhile.
    async fn wait_retry(&mut self) -> bool {
        self.attempts += 1;
        let at = Instant::now() + self.retry;
        let end = tokio::select! {
            biased;
            () = self.cancel.cancelled() => SessionEnd::new("cancelled"),
            () = until(self.deadline) => self.duration_end(),
            () = tokio::time::sleep_until(at.into()) => return true,
        };
        self.stop(end);
        false
    }

    fn duration_end(&self) -> SessionEnd {
        let ms = self.bounds.map_or(0, |b| b.duration.as_millis());
        SessionEnd::new(format!("duration reached ({ms} ms)"))
    }

    /// Record how the session ended; the first end wins.
    fn stop(&mut self, end: SessionEnd) {
        self.exchange.end.get_or_insert(end);
    }

    fn note(&mut self, text: &str) {
        self.record(Direction::System, "error", 0, text.to_owned());
    }

    fn record(&mut self, dir: Direction, kind: &str, size: usize, payload: String) {
        let entry = self
            .exchange
            .timeline
            .record(dir, kind, size, payload)
            .clone();
        forward(self.feed.as_ref(), &self.redactor, entry);
    }

    fn push(&mut self, entry: TimelineEntry) {
        self.exchange.timeline.push(entry.clone());
        forward(self.feed.as_ref(), &self.redactor, entry);
    }

    fn finish(mut self, result: Result<(), RealtimeError>, started: Instant) -> RealtimeExchange {
        if let Err(error) = result {
            self.exchange.end.get_or_insert(SessionEnd::new("failed"));
            self.exchange.error = Some(error);
        }
        self.exchange.end.get_or_insert(SessionEnd::new("closed"));
        self.exchange.elapsed = started.elapsed();
        self.exchange
    }
}

/// A successful event stream, or why not.
fn check(response: &StreamedResponse) -> Result<(), RealtimeError> {
    if !(200..300).contains(&response.status) {
        let reason = http::StatusCode::from_u16(response.status)
            .ok()
            .and_then(|s| s.canonical_reason());
        let shown = match reason {
            Some(reason) => format!("{} {reason}", response.status),
            None => response.status.to_string(),
        };
        return Err(RealtimeError::Status(shown));
    }
    let media = response.header("content-type").unwrap_or_default();
    if !is_event_stream(media) {
        let shown = if media.is_empty() { "(none)" } else { media };
        return Err(RealtimeError::ContentType(shown.to_owned()));
    }
    Ok(())
}

fn handshake_of(response: &StreamedResponse) -> Handshake {
    Handshake {
        status: response.status,
        headers: header_pairs(&response.headers),
        subprotocol: None,
        body: None,
    }
}

fn has_header(request: &ResolvedRequest, name: &str) -> bool {
    request
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case(name))
}
