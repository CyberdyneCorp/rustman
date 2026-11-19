//! Exchange errors of the JSON-RPC client: the request produced no usable
//! JSON-RPC answer. Messages follow the spec texts and never contain
//! unredacted secrets.

use rustman_core::Redactor;
use rustman_http::RequestError;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JsonrpcError {
    /// DNS, connect, TLS, proxy or a timeout; the message is redacted.
    #[error(transparent)]
    Transport(#[from] RequestError),
    /// A non-2xx HTTP status whose body is not a JSON-RPC response.
    #[error("HTTP {0}")]
    Http(u16),
    /// A body or frame that is not a JSON-RPC 2.0 response.
    #[error("invalid JSON-RPC response: {0}")]
    Invalid(String),
    /// The response to a single call carries another id.
    #[error("response id {got} does not match request id {expected}")]
    IdMismatch { got: String, expected: String },
    /// The WebSocket handshake failed or the connection broke.
    #[error("{0}")]
    WebSocket(String),
    /// A request this transport cannot send.
    #[error("{0}")]
    Unsupported(String),
    /// An answer larger than `[http] max_body_bytes`, refused unread. It
    /// would be as large again, so it is never retried.
    #[error("{0}")]
    TooLarge(String),
}

impl JsonrpcError {
    /// An HTTP response body larger than `limit` bytes.
    pub fn body_too_large(limit: u64) -> Self {
        Self::TooLarge(format!(
            "HTTP: the response body is larger than the {limit} byte limit (max_body_bytes)"
        ))
    }

    /// A WebSocket message larger than `limit` bytes.
    pub fn message_too_large(limit: usize) -> Self {
        Self::TooLarge(format!(
            "WebSocket: a message is larger than the {limit} byte limit (max_body_bytes)"
        ))
    }
}

impl JsonrpcError {
    /// The same error with every secret `redactor` knows replaced.
    #[must_use]
    pub fn redacted(self, redactor: &Redactor) -> Self {
        let clean = |text: String| redactor.text(&text).into_owned();
        match self {
            Self::Transport(e) => Self::Transport(e.redacted(redactor)),
            Self::Invalid(m) => Self::Invalid(clean(m)),
            Self::WebSocket(m) => Self::WebSocket(clean(m)),
            Self::IdMismatch { got, expected } => Self::IdMismatch {
                got: clean(got),
                expected: clean(expected),
            },
            other => other,
        }
    }

    /// Whether an idempotent call may be sent again after this error:
    /// connection failures, timeouts and broken WebSockets.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(e) => {
                matches!(e, RequestError::Network(_) | RequestError::Timeout { .. })
            }
            Self::Http(status) => crate::retry::is_retryable_status(*status),
            Self::WebSocket(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    #[test]
    fn messages_follow_the_spec() {
        assert_eq!(JsonrpcError::Http(502).to_string(), "HTTP 502");
        let mismatch = JsonrpcError::IdMismatch {
            got: "2".into(),
            expected: "1".into(),
        };
        assert_eq!(
            mismatch.to_string(),
            "response id 2 does not match request id 1"
        );
        assert_eq!(
            JsonrpcError::Invalid("both result and error present".into()).to_string(),
            "invalid JSON-RPC response: both result and error present"
        );
    }

    #[test]
    fn messages_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("key", "s3cr3t".to_owned()));
        let err = JsonrpcError::WebSocket("closed by s3cr3t".into()).redacted(&redactor);
        assert_eq!(err.to_string(), "closed by «redacted:key»");
    }

    #[test]
    fn transient_errors() {
        assert!(JsonrpcError::Http(429).is_transient());
        assert!(!JsonrpcError::Http(500).is_transient());
        assert!(JsonrpcError::Transport(RequestError::Network("x".into())).is_transient());
        assert!(!JsonrpcError::Invalid("x".into()).is_transient());
        assert!(!JsonrpcError::body_too_large(10).is_transient());
        assert!(!JsonrpcError::message_too_large(10).is_transient());
    }
}
