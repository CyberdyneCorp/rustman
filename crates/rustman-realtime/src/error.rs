//! Why a realtime session failed. Messages follow the spec texts
//! (`ws: handshake failed: 401 Unauthorized`, `ws: not connected`); they
//! may quote interpolated values, so callers redact them before showing
//! or recording them.

use std::time::Duration;

use rustman_http::RequestError;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RealtimeError {
    /// DNS, connect, proxy or TLS; the message is already redacted.
    #[error("ws: {0}")]
    Transport(RequestError),
    /// The server answered the upgrade with another status than 101.
    #[error("ws: handshake failed: {status}{}", reason.as_deref().map(|r| format!(" {r}")).unwrap_or_default())]
    Refused { status: u16, reason: Option<String> },
    /// The handshake broke the protocol, or could not be built.
    #[error("ws: {0}")]
    Handshake(String),
    #[error("ws: handshake timed out after {} ms", .0.as_millis())]
    HandshakeTimedOut(Duration),
    /// A frame was to be sent on a closed socket.
    #[error("ws: not connected")]
    NotConnected,
    /// A `wait_for` step saw no matching message in time.
    #[error("ws: wait_for timed out after {ms} ms ({matcher})")]
    WaitTimedOut { ms: u64, matcher: String },
    /// The connection closed while a `wait_for` step was waiting.
    #[error("ws: connection closed during wait_for ({0})")]
    ClosedWhileWaiting(String),
    /// An incoming message over `limits.max_message_bytes`.
    #[error("ws: message too large: {0} bytes")]
    TooLarge(usize),
    /// A step that cannot run (invalid base64, unreadable file, invalid
    /// matcher).
    #[error("ws: {0}")]
    Step(String),
    /// The connection broke.
    #[error("ws: {0}")]
    Socket(String),
    /// SSE: DNS, connect, proxy, TLS or a broken stream; already redacted.
    #[error("sse: {0}")]
    SseTransport(RequestError),
    /// SSE: the response is not an event stream.
    #[error("sse: unexpected content type {0}")]
    ContentType(String),
    /// SSE: the response status is not a success (nor `204`).
    #[error("sse: unexpected status {0}")]
    Status(String),
    /// SSE: an event over `limits.max_message_bytes`.
    #[error("sse: message too large: {0} bytes")]
    SseTooLarge(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_follow_the_spec() {
        let refused = RealtimeError::Refused {
            status: 401,
            reason: Some("Unauthorized".into()),
        };
        assert_eq!(
            refused.to_string(),
            "ws: handshake failed: 401 Unauthorized"
        );
        let bare = RealtimeError::Refused {
            status: 599,
            reason: None,
        };
        assert_eq!(bare.to_string(), "ws: handshake failed: 599");
        assert_eq!(RealtimeError::NotConnected.to_string(), "ws: not connected");
        let wait = RealtimeError::WaitTimedOut {
            ms: 300,
            matcher: "regex pong".into(),
        };
        assert_eq!(
            wait.to_string(),
            "ws: wait_for timed out after 300 ms (regex pong)"
        );
        assert_eq!(
            RealtimeError::TooLarge(2_097_152).to_string(),
            "ws: message too large: 2097152 bytes"
        );
        assert_eq!(
            RealtimeError::ContentType("application/json".into()).to_string(),
            "sse: unexpected content type application/json"
        );
        assert_eq!(
            RealtimeError::SseTooLarge(2_097_152).to_string(),
            "sse: message too large: 2097152 bytes"
        );
    }
}
