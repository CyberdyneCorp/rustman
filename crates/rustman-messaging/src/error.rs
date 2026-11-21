//! Errors of a messaging operation. Every message is redacted with the
//! request's redactor before it leaves the crate.

use rustman_core::Redactor;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MessagingError {
    /// A request the client cannot run as written (a bad timestamp,
    /// base64 or partition).
    #[error("{0}")]
    Invalid(String),
    /// `kafka: unknown topic '<name>'`.
    #[error("kafka: unknown topic '{0}'")]
    UnknownTopic(String),
    /// `kafka: authentication failed (<mechanism>)`.
    #[error("{protocol}: authentication failed ({mechanism})")]
    AuthFailed {
        protocol: &'static str,
        mechanism: &'static str,
    },
    /// The broker could not be reached or answered with an error.
    #[error("{protocol}: {message}")]
    Broker {
        protocol: &'static str,
        message: String,
    },
    /// A TLS setting could not be loaded.
    #[error("{0}")]
    Tls(String),
    /// A payload file could not be read.
    #[error("{0}")]
    Io(String),
}

impl MessagingError {
    pub fn broker(protocol: &'static str, message: impl Into<String>) -> Self {
        Self::Broker {
            protocol,
            message: message.into(),
        }
    }

    /// The same error with every secret of `redactor` replaced.
    #[must_use]
    pub fn redacted(self, redactor: &Redactor) -> Self {
        let text = |s: String| redactor.text(&s).into_owned();
        match self {
            Self::Invalid(m) => Self::Invalid(text(m)),
            Self::UnknownTopic(t) => Self::UnknownTopic(text(t)),
            Self::Broker { protocol, message } => Self::Broker {
                protocol,
                message: text(message),
            },
            Self::Tls(m) => Self::Tls(text(m)),
            Self::Io(m) => Self::Io(text(m)),
            auth @ Self::AuthFailed { .. } => auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::secret::Secret;

    use super::*;

    #[test]
    fn messages_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("kafka_pw", "pw-123456".to_owned()));
        let err = MessagingError::broker("kafka", "connect pw-123456 failed").redacted(&redactor);
        assert_eq!(err.to_string(), "kafka: connect «redacted:kafka_pw» failed");
        let auth = MessagingError::AuthFailed {
            protocol: "kafka",
            mechanism: "SCRAM-SHA-512",
        };
        assert_eq!(
            auth.redacted(&redactor).to_string(),
            "kafka: authentication failed (SCRAM-SHA-512)"
        );
    }
}
