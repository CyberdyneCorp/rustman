//! What the proxy records (design D5): full exchanges, opaque tunnels and
//! rejected TLS handshakes. A capture holds the exchange unredacted, in
//! memory only; every outward path redacts it (see [`Capture::redactor`]).

use std::fmt;
use std::time::{Duration, SystemTime};

use rustman_core::Redactor;

/// The hint shown with a `TLS REJECTED` entry (spec: Certificate pinning
/// failures).
pub const TLS_REJECTED_HINT: &str =
    "client rejected the Rustman CA (not installed or pinned); exclude the host or install the CA";

/// One header, as received.
pub type Header = (String, String);

/// The stored part of a body: at most the configured `max_body` bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyCapture {
    pub bytes: Vec<u8>,
    /// The size of the whole body, as proxied.
    pub total: u64,
    /// Only the first `bytes.len()` bytes were kept.
    pub truncated: bool,
}

impl BodyCapture {
    /// Add `chunk`, keeping at most `limit` bytes in total.
    pub fn push(&mut self, chunk: &[u8], limit: usize) {
        self.total += chunk.len() as u64;
        let room = limit.saturating_sub(self.bytes.len());
        if chunk.len() > room {
            self.truncated = true;
        }
        self.bytes
            .extend_from_slice(&chunk[..room.min(chunk.len())]);
    }
}

/// One request and its response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub method: String,
    /// The absolute URL the request went to.
    pub url: String,
    /// The client's HTTP version (`HTTP/1.1`, `HTTP/2`).
    pub version: String,
    pub request_headers: Vec<Header>,
    pub request_body: BodyCapture,
    /// `None` when no response arrived (see `error`).
    pub status: Option<u16>,
    pub response_headers: Vec<Header>,
    pub response_body: BodyCapture,
    pub duration: Duration,
    /// Decrypted from an intercepted `CONNECT` tunnel.
    pub intercepted: bool,
    /// Why the upstream exchange failed.
    pub error: Option<String>,
}

/// A `CONNECT` tunnel relayed without decryption: only its endpoint and
/// byte counts are known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tunnel {
    pub host: String,
    pub port: u16,
    /// Bytes from the client to the server.
    pub bytes_up: u64,
    /// Bytes from the server to the client.
    pub bytes_down: u64,
    pub duration: Duration,
    /// Why the tunnel could not be opened or ended abnormally.
    pub error: Option<String>,
}

/// What one capture recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    Exchange(Box<Exchange>),
    Tunnel(Tunnel),
    /// A client aborted the TLS handshake against an intercepted host.
    TlsRejected {
        host: String,
        port: u16,
    },
}

/// One entry of the capture list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Increasing within one proxy session.
    pub id: u64,
    pub started: SystemTime,
    pub recorded: Recorded,
}

impl AsRef<Capture> for Capture {
    fn as_ref(&self) -> &Capture {
        self
    }
}

impl Capture {
    /// The exchange this capture holds, if any.
    pub fn exchange(&self) -> Option<&Exchange> {
        match &self.recorded {
            Recorded::Exchange(exchange) => Some(exchange),
            _ => None,
        }
    }

    /// The bytes this capture keeps in memory (both bodies).
    pub fn stored_bytes(&self) -> usize {
        self.exchange().map_or(0, |e| {
            e.request_body.bytes.len() + e.response_body.bytes.len()
        })
    }

    /// A redactor for everything shown or exported about this capture
    /// (live lines, `--json`, the TUI, HAR): the credentials its sensitive
    /// headers carry and the values of its secret-named headers, query
    /// parameters and body fields are redacted wherever they reappear (an
    /// echoing server, a URL).
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::new();
        if let Some(exchange) = self.exchange() {
            let headers = exchange
                .request_headers
                .iter()
                .chain(&exchange.response_headers);
            for (name, value) in headers {
                redactor.add_header_value(name, value);
            }
            crate::named::add_named_secrets(&mut redactor, exchange);
        }
        redactor
    }
}

/// The one-line summary printed by `rustman record`, unredacted; callers
/// pass it through [`Capture::redactor`].
impl fmt::Display for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.recorded {
            Recorded::Exchange(e) => write_exchange(f, e),
            Recorded::Tunnel(t) => {
                write!(
                    f,
                    "TUNNEL {}:{} {} B up, {} B down",
                    t.host, t.port, t.bytes_up, t.bytes_down
                )?;
                match &t.error {
                    Some(error) => write!(f, " ({error})"),
                    None => Ok(()),
                }
            }
            Recorded::TlsRejected { host, .. } => {
                write!(f, "TLS REJECTED {host}: {TLS_REJECTED_HINT}")
            }
        }
    }
}

fn write_exchange(f: &mut fmt::Formatter<'_>, e: &Exchange) -> fmt::Result {
    let status = e.status.map_or_else(|| "---".to_owned(), |s| s.to_string());
    write!(
        f,
        "{} {} {status} {} ms {} B",
        e.method,
        e.url,
        e.duration.as_millis(),
        e.response_body.total
    )?;
    if e.response_body.truncated || e.request_body.truncated {
        f.write_str(" truncated")?;
    }
    match &e.error {
        Some(error) => write!(f, " ({error})"),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange() -> Exchange {
        Exchange {
            method: "GET".into(),
            url: "http://api.test/v1/me?k=abcdef123".into(),
            version: "HTTP/1.1".into(),
            request_headers: vec![("Authorization".into(), "Bearer abcdef123".into())],
            request_body: BodyCapture::default(),
            status: Some(200),
            response_headers: Vec::new(),
            response_body: BodyCapture::default(),
            duration: Duration::from_millis(12),
            intercepted: false,
            error: None,
        }
    }

    fn capture(recorded: Recorded) -> Capture {
        Capture {
            id: 1,
            started: SystemTime::UNIX_EPOCH,
            recorded,
        }
    }

    #[test]
    fn bodies_keep_at_most_the_limit() {
        let mut body = BodyCapture::default();
        body.push(b"hello", 8);
        body.push(b"world", 8);
        assert_eq!(body.bytes, b"hellowor");
        assert_eq!(body.total, 10);
        assert!(body.truncated);
    }

    #[test]
    fn summaries_name_each_kind() {
        let line = capture(Recorded::Exchange(Box::new(exchange()))).to_string();
        assert_eq!(line, "GET http://api.test/v1/me?k=abcdef123 200 12 ms 0 B");
        let tunnel = Tunnel {
            host: "www.google.com".into(),
            port: 443,
            bytes_up: 10,
            bytes_down: 20,
            duration: Duration::ZERO,
            error: None,
        };
        assert_eq!(
            capture(Recorded::Tunnel(tunnel)).to_string(),
            "TUNNEL www.google.com:443 10 B up, 20 B down"
        );
        let rejected = Recorded::TlsRejected {
            host: "api.example.com".into(),
            port: 443,
        };
        assert_eq!(
            capture(rejected).to_string(),
            format!("TLS REJECTED api.example.com: {TLS_REJECTED_HINT}")
        );
    }

    #[test]
    fn the_redactor_hides_header_credentials_everywhere() {
        let capture = capture(Recorded::Exchange(Box::new(exchange())));
        let shown = capture.redactor().text(&capture.to_string()).into_owned();
        assert!(!shown.contains("abcdef123"), "{shown}");
    }
}
