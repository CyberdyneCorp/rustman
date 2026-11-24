//! An open WebSocket and the exchange it is recording: reading the next
//! event, sending frames, answering pings, measuring pong round trips and
//! closing.

use std::collections::VecDeque;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use rustman_core::Redactor;
use rustman_core::model::CloseStep;
use rustman_core::vars::{ResolvedFrame, ResolvedRequest};
use rustman_http::WebSocket;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::error::{CapacityError, ProtocolError};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_util::sync::CancellationToken;

use super::CLOSE_WAIT;
use super::frames::{outgoing, shown_binary, text_kind};
use crate::error::RealtimeError;
use crate::exchange::{RealtimeExchange, SessionEnd};
use crate::feed::FeedSender;
use crate::session::{Bounds, Command, SessionOptions, forward, recv, until};
use crate::timeline::{Direction, TimelineEntry};
use rustman_core::model::MAX_CLOSE_REASON;

/// What reading the session produced.
pub(super) enum Event {
    /// A data message: its text, or `None` for binary data.
    Message(Option<String>),
    /// The connection closed (by the server, or it dropped).
    Closed,
    /// The deadline passed first.
    Deadline,
    Cancelled,
    Command(Command),
}

/// A session in progress.
pub(super) struct Live {
    socket: WebSocket,
    exchange: RealtimeExchange,
    /// `false` once a close was sent or received, or the connection broke.
    open: bool,
    /// Our pings waiting for their pong, with when they left.
    pings: VecDeque<(Vec<u8>, Instant)>,
    redactor: Redactor,
    /// `limits.max_message_bytes`: also the largest file a binary frame
    /// may send.
    max_message_bytes: u64,
    started: Instant,
    bounds: Option<Bounds>,
    cancel: CancellationToken,
    feed: Option<FeedSender>,
    commands: Option<mpsc::Receiver<Command>>,
}

impl Live {
    pub(super) fn new(
        socket: WebSocket,
        exchange: RealtimeExchange,
        http: ResolvedRequest,
        max_message_bytes: u64,
        session: SessionOptions,
    ) -> Self {
        let mut redactor = Redactor::for_request(&http);
        redactor.merge(&exchange.credentials);
        Self {
            socket,
            exchange,
            open: true,
            pings: VecDeque::new(),
            redactor,
            max_message_bytes,
            started: Instant::now(),
            bounds: session.bounds,
            cancel: session.cancel,
            feed: session.feed,
            commands: session.commands,
        }
    }

    pub(super) fn bounds(&self) -> Option<Bounds> {
        self.bounds
    }

    pub(super) fn started(&self) -> Instant {
        self.started
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn received(&self) -> u64 {
        self.exchange.received
    }

    /// Record the connection in the timeline.
    pub(super) fn connected(&mut self) {
        let note = match self.exchange.subprotocol() {
            Some(protocol) => format!("connected (subprotocol {protocol})"),
            None => "connected".to_owned(),
        };
        self.record(Direction::System, "connect", 0, note);
    }

    /// Record an entry and feed it, redacted, to whoever follows the
    /// session.
    pub(super) fn record(&mut self, dir: Direction, kind: &str, size: usize, payload: String) {
        let entry = self
            .exchange
            .timeline
            .record(dir, kind, size, payload)
            .clone();
        forward(self.feed.as_ref(), &self.redactor, entry);
    }

    /// The next event before `deadline` (`None`: no deadline). Pings are
    /// answered and pongs timed on the way.
    pub(super) async fn next(&mut self, deadline: Option<Instant>) -> Result<Event, RealtimeError> {
        loop {
            let incoming = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Ok(Event::Cancelled),
                () = until(deadline) => return Ok(Event::Deadline),
                command = recv(&mut self.commands) => match command {
                    Some(command) => return Ok(Event::Command(command)),
                    None => {
                        self.commands = None;
                        continue;
                    }
                },
                incoming = self.socket.next() => incoming,
            };
            if let Some(event) = self.on_incoming(incoming).await? {
                return Ok(event);
            }
        }
    }

    async fn on_incoming(
        &mut self,
        incoming: Option<Result<Message, WsError>>,
    ) -> Result<Option<Event>, RealtimeError> {
        match incoming {
            Some(Ok(message)) => Ok(self.on_message(message)),
            Some(Err(WsError::Capacity(CapacityError::MessageTooLong { size, .. }))) => {
                self.too_large(size).await;
                Err(RealtimeError::TooLarge(size))
            }
            Some(Err(
                WsError::Io(_) | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
            )) => {
                self.lost("connection dropped");
                Ok(Some(Event::Closed))
            }
            Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) | None => {
                self.lost("connection closed");
                Ok(Some(Event::Closed))
            }
            Some(Err(e)) => {
                self.open = false;
                Err(RealtimeError::Socket(e.to_string()))
            }
        }
    }

    fn on_message(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::Text(text) => {
                let text = text.as_str().to_owned();
                self.exchange.received_one();
                self.record(
                    Direction::Received,
                    text_kind(&text),
                    text.len(),
                    text.clone(),
                );
                Some(Event::Message(Some(text)))
            }
            Message::Binary(bytes) => {
                self.exchange.received_one();
                let shown = shown_binary(&bytes);
                self.record(Direction::Received, "binary", bytes.len(), shown);
                Some(Event::Message(None))
            }
            Message::Ping(data) => {
                let shown = shown_binary(&data);
                self.record(Direction::Received, "ping", data.len(), shown.clone());
                // tungstenite queues the pong and sends it on the next read.
                self.record(Direction::Sent, "pong", data.len(), shown);
                None
            }
            Message::Pong(data) => {
                self.pong(&data);
                None
            }
            Message::Close(frame) => {
                self.server_closed(frame);
                Some(Event::Closed)
            }
            Message::Frame(_) => None,
        }
    }

    /// Record a pong, with its round-trip time when it answers our ping.
    fn pong(&mut self, data: &[u8]) {
        let answered = self.pings.iter().position(|(sent, _)| sent == data);
        let rtt = answered
            .and_then(|i| self.pings.remove(i))
            .map(|(_, at)| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX));
        let entry = TimelineEntry {
            dir: Direction::Received,
            t_ms: self.exchange.timeline.now_ms(),
            size: data.len() as u64,
            kind: "pong".to_owned(),
            payload: shown_binary(data),
            id: None,
            rtt_ms: rtt,
        };
        self.exchange.timeline.push(entry.clone());
        forward(self.feed.as_ref(), &self.redactor, entry);
    }

    fn server_closed(&mut self, frame: Option<CloseFrame>) {
        let (code, reason) = match &frame {
            Some(f) => (Some(u16::from(f.code)), f.reason.as_str().to_owned()),
            None => (None, String::new()),
        };
        let shown = close_text(code, &reason);
        self.record(Direction::Received, "close", reason.len() + 2, shown);
        if self.exchange.end.is_none() {
            self.exchange.end = Some(match code {
                Some(code) => SessionEnd::closed_by("server", code, &reason),
                None => SessionEnd::new("closed by server"),
            });
        }
        self.open = false;
    }

    fn lost(&mut self, why: &str) {
        if self.open {
            self.record(Direction::System, "close", 0, why.to_owned());
        }
        self.open = false;
        if self.exchange.end.is_none() {
            self.exchange.end = Some(SessionEnd::new(why));
        }
    }

    /// A message over the limit: record it and close with 1009 without
    /// keeping the rest of it. The close code is recorded only when the
    /// close frame could be sent.
    async fn too_large(&mut self, size: usize) {
        let note = format!("message too large: {size} bytes");
        self.record(Direction::System, "close", 0, note.clone());
        let reason = "message too large";
        let sent = self.open
            && self
                .socket
                .send(Message::Close(Some(close_frame(1009, reason))))
                .await
                .is_ok();
        self.open = false;
        self.exchange.end = Some(SessionEnd {
            reason: note,
            close_code: sent.then_some(1009),
            close_reason: sent.then(|| reason.to_owned()),
        });
        if sent {
            self.drain_until_closed().await;
        }
    }

    /// After our close frame, read and discard what the server still
    /// sends until it ends the connection, for at most [`CLOSE_WAIT`]
    /// (RFC 6455 section 7.1.1: the client waits for the server to close
    /// TCP first). Dropping a socket with unread bytes resets the
    /// connection, and a reset can make the server's TCP stack discard our
    /// close frame before the server reads it (as Windows does). The
    /// bytes are read below the WebSocket layer, which cannot go on after
    /// a message over the limit.
    async fn drain_until_closed(&mut self) {
        use tokio::io::AsyncReadExt as _;
        let deadline = tokio::time::Instant::now() + CLOSE_WAIT;
        let mut buffer = vec![0; 64 * 1024];
        let io = self.socket.get_mut();
        while let Ok(Ok(read)) = tokio::time::timeout_at(deadline, io.read(&mut buffer)).await {
            if read == 0 {
                return;
            }
        }
    }

    /// Send `frame`; `ws: not connected` once the socket is closed.
    pub(super) async fn send(&mut self, frame: &ResolvedFrame) -> Result<(), RealtimeError> {
        if !self.open {
            return Err(RealtimeError::NotConnected);
        }
        let out = outgoing(frame, self.max_message_bytes).await?;
        if let Message::Ping(data) = &out.message {
            self.pings.push_back((data.to_vec(), Instant::now()));
        }
        self.socket.send(out.message).await.map_err(|e| match e {
            WsError::ConnectionClosed | WsError::AlreadyClosed => RealtimeError::NotConnected,
            other => RealtimeError::Socket(other.to_string()),
        })?;
        self.record(Direction::Sent, out.kind, out.size, out.payload);
        Ok(())
    }

    /// Send a close frame, then wait up to [`CLOSE_WAIT`] for the
    /// server's. `end` replaces the default `closed by client: <code>`.
    pub(super) async fn close(&mut self, step: &CloseStep, end: Option<String>) {
        if !self.open {
            return;
        }
        self.open = false;
        let code = step.effective_code();
        let reason = step.reason.clone().unwrap_or_default();
        let sent = self
            .socket
            .send(Message::Close(Some(close_frame(code, &reason))))
            .await;
        if sent.is_ok() {
            let shown = close_text(Some(code), &reason);
            self.record(Direction::Sent, "close", reason.len() + 2, shown);
        }
        let mut closed = SessionEnd::closed_by("client", code, &reason);
        if let Some(end) = end {
            closed.reason = end;
        }
        self.exchange.end.get_or_insert(closed);
        self.await_close().await;
    }

    /// Read until the server's close frame or the end of the stream, for
    /// at most [`CLOSE_WAIT`]. Data frames the server sent before it saw
    /// our close are discarded: the session already ended.
    async fn await_close(&mut self) {
        let deadline = tokio::time::Instant::now() + CLOSE_WAIT;
        loop {
            let read = tokio::time::timeout_at(deadline, self.socket.next()).await;
            match read {
                Ok(Some(Ok(Message::Close(frame)))) => {
                    self.server_closed(frame);
                    return;
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_)) | None) | Err(_) => return,
            }
        }
    }

    /// Perform `command`. A frame that cannot be sent is noted in the
    /// timeline; the session goes on.
    pub(super) async fn command(&mut self, command: Command) {
        match command {
            Command::Send(frame) => {
                if let Err(e) = self.send(&frame).await {
                    self.record(Direction::System, "error", 0, e.to_string());
                }
            }
            Command::Close(step) => self.close(&step, None).await,
        }
    }

    /// End the session: on `result`'s error, close normally first. Gives
    /// the server's close a chance to complete, then returns the exchange.
    pub(super) async fn finish(mut self, result: Result<(), RealtimeError>) -> RealtimeExchange {
        if let Err(error) = result {
            self.close(&CloseStep::default(), None).await;
            self.exchange.error = Some(error);
        }
        if self.exchange.end.is_none() {
            self.exchange.end = Some(SessionEnd::new("closed"));
        }
        // Flush a close reply tungstenite queued for the server.
        let _ = tokio::time::timeout(CLOSE_WAIT, self.socket.close(None)).await;
        self.exchange
    }
}

/// A close frame; the reason is cut, at a character boundary, to the 123
/// bytes a control frame leaves for it (a file's reason is checked when it
/// loads, but placeholders may lengthen it).
fn close_frame(code: u16, reason: &str) -> CloseFrame {
    let mut end = reason.len().min(MAX_CLOSE_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    CloseFrame {
        code: code.into(),
        reason: reason[..end].to_owned().into(),
    }
}

/// A close frame as the timeline shows it: `1008 policy`.
fn close_text(code: Option<u16>, reason: &str) -> String {
    match code {
        Some(code) if reason.is_empty() => code.to_string(),
        Some(code) => format!("{code} {reason}"),
        None => reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_close_reason_is_cut_at_a_character_boundary() {
        let reason = format!("{}é", "r".repeat(122));
        let frame = close_frame(1000, &reason);
        assert_eq!(frame.reason.as_str(), "r".repeat(122));
        assert_eq!(close_frame(1000, "bye").reason.as_str(), "bye");
    }
}
