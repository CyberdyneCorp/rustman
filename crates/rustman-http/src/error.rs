//! Request errors (design D12). Every message passes through the
//! [`Redactor`] before it leaves the engine.

use std::fmt;
use std::time::Duration;

use rustman_core::Redactor;

/// The phase a timeout fired in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// DNS, TCP connect, proxy tunnel or TLS handshake.
    Connect,
    /// Waiting for the response or reading its body.
    Response,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::Response => "response",
        })
    }
}

/// Why a request produced no exchange.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The request could not be built (bad URL, header or body file).
    #[error("{0}")]
    Invalid(String),
    /// DNS failure, refused connection, reset, protocol error.
    #[error("{0}")]
    Network(String),
    /// A TLS configuration or handshake failure; starts with `tls:`.
    #[error("{0}")]
    Tls(String),
    #[error("timeout after {} ms ({phase})", .after.as_millis())]
    Timeout { after: Duration, phase: Phase },
    /// Reading a file the request needs.
    #[error("{0}")]
    Io(String),
    /// A proxy refused or failed to set up the tunnel; starts with `proxy:`.
    #[error("{0}")]
    Proxy(String),
    /// Acquiring or refreshing credentials failed; starts with `oauth2:`.
    #[error("{0}")]
    Auth(String),
    /// More redirects than `max_redirects` allows.
    #[error("too many redirects ({0})")]
    TooManyRedirects(u32),
}

impl RequestError {
    /// The same error with every secret value replaced by its placeholder.
    pub fn redacted(self, redactor: &Redactor) -> Self {
        let clean = |text: String| redactor.text(&text).into_owned();
        match self {
            Self::Invalid(m) => Self::Invalid(clean(m)),
            Self::Network(m) => Self::Network(clean(m)),
            Self::Tls(m) => Self::Tls(clean(m)),
            Self::Io(m) => Self::Io(clean(m)),
            Self::Proxy(m) => Self::Proxy(clean(m)),
            Self::Auth(m) => Self::Auth(clean(m)),
            other @ (Self::Timeout { .. } | Self::TooManyRedirects(_)) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    #[test]
    fn timeout_message_names_the_phase() {
        let err = RequestError::Timeout {
            after: Duration::from_millis(500),
            phase: Phase::Response,
        };
        assert_eq!(err.to_string(), "timeout after 500 ms (response)");
        let err = RequestError::Timeout {
            after: Duration::from_secs(10),
            phase: Phase::Connect,
        };
        assert_eq!(err.to_string(), "timeout after 10000 ms (connect)");
    }

    #[test]
    fn too_many_redirects_names_the_limit() {
        assert_eq!(
            RequestError::TooManyRedirects(10).to_string(),
            "too many redirects (10)"
        );
    }

    #[test]
    fn messages_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("token", "s3cr3t".to_owned()));
        let err = RequestError::Network("cannot reach s3cr3t.example".into()).redacted(&redactor);
        assert_eq!(err.to_string(), "cannot reach «redacted:token».example");
        let err = RequestError::Proxy("proxy: s3cr3t refused".into()).redacted(&redactor);
        assert_eq!(err.to_string(), "proxy: «redacted:token» refused");
        let err = RequestError::Auth("oauth2: bad s3cr3t".into()).redacted(&redactor);
        assert_eq!(err.to_string(), "oauth2: bad «redacted:token»");
    }
}
