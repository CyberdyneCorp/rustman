//! What a session produced (design D5): the handshake, the timeline, how
//! it ended and why it failed, if it did. Nothing here is redacted: the
//! runner and the front ends redact what they show or record.

use std::time::{Duration, Instant};

use rustman_core::Redactor;
use rustman_core::vars::ResolvedRequest;

use crate::error::RealtimeError;
use crate::timeline::Timeline;

/// The server's answer to the opening request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handshake {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The subprotocol the server chose (WebSocket).
    pub subprotocol: Option<String>,
    /// The body of a refused upgrade as text: the record cuts it to 64 KiB
    /// once redacted.
    pub body: Option<String>,
}

impl Handshake {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEnd {
    /// `closed by server: 1008 policy`, `closed by client: 1000`,
    /// `max_messages reached (50)`, `duration reached (30000 ms)`...
    pub reason: String,
    pub close_code: Option<u16>,
    pub close_reason: Option<String>,
}

impl SessionEnd {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            close_code: None,
            close_reason: None,
        }
    }

    /// A close frame from `side` (`server` or `client`).
    pub fn closed_by(side: &str, code: u16, reason: &str) -> Self {
        let shown = if reason.is_empty() {
            format!("closed by {side}: {code}")
        } else {
            format!("closed by {side}: {code} {reason}")
        };
        Self {
            reason: shown,
            close_code: Some(code),
            close_reason: (!reason.is_empty()).then(|| reason.to_owned()),
        }
    }
}

/// A finished session.
#[derive(Clone, Debug)]
pub struct RealtimeExchange {
    /// `ws` or `sse`.
    pub protocol: &'static str,
    /// The request that opened the session, as sent.
    pub request: ResolvedRequest,
    pub handshake: Option<Handshake>,
    pub timeline: Timeline,
    pub end: Option<SessionEnd>,
    pub error: Option<RealtimeError>,
    /// Messages or events received, including those dropped from the
    /// timeline.
    pub received: u64,
    /// When the first message or event arrived, in ms since the start (the
    /// `latency` the assertions read).
    pub first_message_ms: Option<u64>,
    pub elapsed: Duration,
    /// What the opening requests added on the wire (auth helpers, cookies
    /// from the jar) and the credentials their responses set: values to
    /// redact along with the request's secrets.
    pub credentials: Redactor,
}

impl RealtimeExchange {
    pub fn new(
        protocol: &'static str,
        request: ResolvedRequest,
        started: Instant,
        max_timeline: u32,
    ) -> Self {
        Self {
            protocol,
            request,
            handshake: None,
            timeline: Timeline::new(started, max_timeline),
            end: None,
            error: None,
            received: 0,
            first_message_ms: None,
            elapsed: Duration::ZERO,
            credentials: Redactor::new(),
        }
    }

    /// Count a received message or event.
    pub fn received_one(&mut self) {
        self.received += 1;
        if self.first_message_ms.is_none() {
            self.first_message_ms = Some(self.timeline.now_ms());
        }
    }

    /// The negotiated subprotocol, if any.
    pub fn subprotocol(&self) -> Option<&str> {
        self.handshake.as_ref()?.subprotocol.as_deref()
    }

    /// The close code either side sent, if the session closed.
    pub fn close_code(&self) -> Option<u16> {
        self.end.as_ref()?.close_code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reasons_read_as_the_spec_shows_them() {
        let end = SessionEnd::closed_by("server", 1008, "policy");
        assert_eq!(end.reason, "closed by server: 1008 policy");
        assert_eq!(end.close_reason.as_deref(), Some("policy"));
        let bare = SessionEnd::closed_by("client", 1000, "");
        assert_eq!(bare.reason, "closed by client: 1000");
        assert_eq!(bare.close_reason, None);
    }
}
