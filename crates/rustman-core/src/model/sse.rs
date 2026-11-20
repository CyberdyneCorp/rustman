//! The `kind: sse` request file (realtime-sessions "SSE request file"): an
//! `http(s)://` URL, a method and optional body (POST-based streams such
//! as LLM APIs), headers, auth, the initial `Last-Event-ID`, the reconnect
//! policy and limits.
//!
//! Sessions run in the `rustman-realtime` crate; this module only holds
//! the file model and its load-time checks.

use std::path::Path;

use serde::Deserialize;

use super::assertion::Assertion;
use super::auth::Auth;
use super::body::Body;
use super::capture::Capture;
use super::common::{Checked, NameValue, Variables};
use super::http::{Method, RequestSettings};
use super::ws::{RealtimeLimits, RealtimeTls, check_scheme, tls_paths};
use crate::error::LoadError;
use crate::format::{Located, from_yaml};

/// The scheme error of a `kind: sse` URL.
pub const SSE_SCHEME_ERROR: &str = "sse: url must use http:// or https://";

/// Sent as `Accept` unless the request sets its own.
pub const SSE_ACCEPT: &str = "text/event-stream";

/// Reconnect attempts when `reconnect.max_attempts` is not set.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// The reconnect delay before the server sends a `retry` field.
pub const DEFAULT_RETRY_MS: u64 = 3_000;

/// The longest reconnect delay a server's `retry` field may set (60 s).
pub const MAX_RETRY_MS: u64 = 60_000;

/// A `kind: sse` request file. Saving writes `source` back unchanged; a
/// request built in code (empty `source`) is written field by field.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SseRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// How the request appears in `rustman docs` output.
    pub docs: Option<super::docs::DocsMeta>,
    /// Defaults to `GET` when absent.
    pub method: Option<Method>,
    /// An `http://` or `https://` URL; `{{variables}}` are interpolated.
    pub url: Located<String>,
    pub headers: Option<Vec<NameValue>>,
    pub auth: Option<Auth>,
    pub body: Option<Body>,
    /// Sent as `Last-Event-ID` on the first connection.
    pub last_event_id: Option<String>,
    pub reconnect: Option<Reconnect>,
    pub limits: Option<Checked<RealtimeLimits>>,
    pub tls: Option<RealtimeTls>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

/// `reconnect`: how many times a dropped stream is resumed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconnect {
    /// `0` disables reconnecting.
    pub max_attempts: Option<u32>,
}

impl Reconnect {
    pub fn effective_max_attempts(&self) -> u32 {
        self.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS)
    }
}

impl SseRequest {
    /// Parse and check the `kind: sse` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        check_scheme(
            file,
            &request.url,
            &["http://", "https://"],
            SSE_SCHEME_ERROR,
        )?;
        request.source = text.to_owned();
        Ok(request)
    }

    /// The method to send: the file's `method`, or `GET`.
    pub fn effective_method(&self) -> &str {
        self.method.as_ref().map_or("GET", Method::as_str)
    }

    /// The reconnect policy, with its defaults.
    pub fn effective_reconnect(&self) -> Reconnect {
        self.reconnect.unwrap_or_default()
    }

    /// The limits, with their defaults.
    pub fn effective_limits(&self) -> RealtimeLimits {
        self.limits.as_deref().cloned().unwrap_or_default()
    }

    /// The TLS settings, as a `kind: http` request has them.
    pub fn http_settings(&self) -> Option<RequestSettings> {
        self.tls.as_ref().map(RealtimeTls::settings)
    }

    /// Every workspace file the request refers to, with its position.
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        let mut paths = tls_paths(self.tls.as_ref());
        if let Some(Body::File(path)) = &self.body {
            paths.push(path);
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "version: 1\nkind: sse\nname: events\n";

    fn parse(text: &str) -> Result<SseRequest, LoadError> {
        SseRequest::parse(Path::new("e.rman.yaml"), text)
    }

    #[test]
    fn a_post_stream_loads_with_defaults() {
        let r = parse(&format!(
            "{HEAD}method: POST\nurl: https://api.example.com/v1/chat\nbody: {{type: json, content: {{stream: true}}}}\nlast_event_id: '41'\n"
        ))
        .unwrap();
        assert_eq!(r.effective_method(), "POST");
        assert_eq!(r.last_event_id.as_deref(), Some("41"));
        assert_eq!(r.effective_reconnect().effective_max_attempts(), 5);
        assert_eq!(r.effective_limits().effective_max_messages(), 50);
        assert!(matches!(r.body, Some(Body::Json(_))));
    }

    #[test]
    fn a_get_stream_is_the_default() {
        let r = parse(&format!(
            "{HEAD}url: http://h/events\nreconnect: {{max_attempts: 0}}\n"
        ))
        .unwrap();
        assert_eq!(r.effective_method(), "GET");
        assert_eq!(r.effective_reconnect().effective_max_attempts(), 0);
    }

    #[test]
    fn a_websocket_url_is_rejected() {
        let err = parse(&format!("{HEAD}url: wss://h/events\n")).unwrap_err();
        assert_eq!(err.message, "sse: url must use http:// or https://");
        let err = parse(&format!("{HEAD}url: http://h/\nsteps: []\n")).unwrap_err();
        assert!(err.message.contains("unknown field"), "{}", err.message);
    }
}
