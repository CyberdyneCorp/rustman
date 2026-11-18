//! The event log (spec: Event log): every capture, rejection and mock match,
//! plus mock reloads, as one line each.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::capture::Capture;

/// A request the listener refused (400, 408, 413 or 431) and did not capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub remote: SocketAddr,
    pub status: u16,
    /// Why, e.g. `too many header fields`.
    pub reason: String,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} rejected {} {}",
            self.remote, self.status, self.reason
        )
    }
}

/// Something that happened on the listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Captured(Arc<Capture>),
    Rejected(Rejection),
    /// A mock answered capture `id`.
    MockMatched {
        id: u64,
        method: String,
        pattern: String,
        status: u16,
        delay_ms: u64,
    },
    /// The mocks file was read again.
    MocksReloaded {
        count: usize,
    },
    /// The mocks file is invalid; the previous mocks stay active.
    MocksInvalid {
        message: String,
    },
    /// The capture buffer was emptied.
    Cleared,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Captured(c) => {
                write!(
                    f,
                    "#{} {} {} from {} ({} B) -> {}",
                    c.id,
                    c.method,
                    c.target(),
                    c.remote,
                    c.body_size(),
                    c.status
                )?;
                match c.signature {
                    Some(signature) => write!(f, " signature: {signature}"),
                    None => Ok(()),
                }
            }
            Self::Rejected(rejection) => rejection.fmt(f),
            Self::MockMatched {
                id,
                method,
                pattern,
                status,
                delay_ms,
            } => {
                write!(f, "#{id} matched mock {method} {pattern} -> {status}")?;
                if *delay_ms > 0 {
                    write!(f, " after {delay_ms} ms")?;
                }
                Ok(())
            }
            Self::MocksReloaded { count } => write!(f, "mocks reloaded: {count} active"),
            Self::MocksInvalid { message } => {
                write!(f, "mocks not reloaded (previous mocks kept): {message}")
            }
            Self::Cleared => f.write_str("captures cleared"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::tests::capture;

    #[test]
    fn a_431_line_names_the_remote_status_and_reason() {
        let event = Event::Rejected(Rejection {
            remote: "127.0.0.1:50123".parse().unwrap(),
            status: 431,
            reason: "too many header fields".into(),
        });
        assert_eq!(
            event.to_string(),
            "127.0.0.1:50123 rejected 431 too many header fields"
        );
    }

    #[test]
    fn capture_and_mock_lines() {
        let mut c = capture("/hooks/github", b"{}");
        c.id = 3;
        c.query = Some("x=1".into());
        c.status = 500;
        assert_eq!(
            Event::Captured(Arc::new(c.clone())).to_string(),
            "#3 POST /hooks/github?x=1 from 127.0.0.1:5000 (2 B) -> 500"
        );
        c.signature = Some(crate::signature::Signature::Invalid);
        assert_eq!(
            Event::Captured(Arc::new(c)).to_string(),
            "#3 POST /hooks/github?x=1 from 127.0.0.1:5000 (2 B) -> 500 signature: invalid"
        );
        let matched = Event::MockMatched {
            id: 3,
            method: "POST".into(),
            pattern: "/hooks/*".into(),
            status: 500,
            delay_ms: 2000,
        };
        assert_eq!(
            matched.to_string(),
            "#3 matched mock POST /hooks/* -> 500 after 2000 ms"
        );
        assert_eq!(
            Event::MocksInvalid {
                message: "bad".into()
            }
            .to_string(),
            "mocks not reloaded (previous mocks kept): bad"
        );
    }
}
