//! The `kind: ws` request file (realtime-sessions "WebSocket request
//! file"): a WebSocket URL, handshake headers, auth and subprotocols, a
//! scripted sequence of steps, headless limits and TLS settings.
//!
//! Sessions run in the `rustman-realtime` crate; this module only holds
//! the file model and its load-time checks. The parts both realtime kinds
//! share (limits, TLS) live here and are reused by [`super::sse`].

use std::fmt;
use std::path::Path;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::assertion::{Assertion, JsonAssertion};
use super::auth::Auth;
use super::capture::Capture;
use super::common::{Checked, NameValue, Validate, Variables, exactly_one, given, seen};
use super::http::RequestSettings;
use crate::error::{LoadError, Position};
use crate::format::{JsonValue, Located, from_yaml};
use crate::secret::Hidden;

/// Received messages after which a headless session ends.
pub const DEFAULT_MAX_MESSAGES: u32 = 50;

/// How long a headless session runs at most.
pub const DEFAULT_DURATION_MS: u64 = 30_000;

/// The largest message or event a session accepts (16 MiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Timeline entries a session keeps; older ones are dropped.
pub const DEFAULT_MAX_TIMELINE: u32 = 10_000;

/// How long a `wait_for` step waits when `timeout_ms` is not set.
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5_000;

/// The close code a session sends when none is given.
pub const NORMAL_CLOSE: u16 = 1000;

/// The scheme error of a `kind: ws` URL.
pub const WS_SCHEME_ERROR: &str = "ws: url must use ws:// or wss://";

/// A `kind: ws` request file. Saving writes `source` back unchanged; a
/// request built in code (empty `source`) is written field by field.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// A `ws://` or `wss://` URL; `{{variables}}` are interpolated.
    pub url: Located<String>,
    pub headers: Option<Vec<NameValue>>,
    pub auth: Option<Auth>,
    /// Offered in order as `Sec-WebSocket-Protocol`.
    pub subprotocols: Option<Vec<String>>,
    pub steps: Option<Vec<Located<Step>>>,
    pub limits: Option<Checked<RealtimeLimits>>,
    pub tls: Option<RealtimeTls>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

impl WsRequest {
    /// Parse and check the `kind: ws` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        check_scheme(file, &request.url, &["ws://", "wss://"], WS_SCHEME_ERROR)?;
        request.source = text.to_owned();
        Ok(request)
    }

    /// The steps, in file order.
    pub fn step_list(&self) -> Vec<Step> {
        self.steps
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|s| s.get().clone())
            .collect()
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
        for step in self.steps.as_deref().unwrap_or_default() {
            if let Step::Send(Frame::Binary(BinarySource::File(path))) = step.get() {
                paths.push(path);
            }
        }
        paths
    }
}

/// Fail with `message` unless `url` starts with one of `schemes` (any
/// case) or with a `{{variable}}`.
pub(super) fn check_scheme(
    file: &Path,
    url: &Located<String>,
    schemes: &[&str],
    message: &str,
) -> Result<(), LoadError> {
    let trimmed = url.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let templated = trimmed.starts_with("{{");
    if templated || schemes.iter().any(|s| lower.starts_with(s)) {
        return Ok(());
    }
    let at = url.position().or(Some(Position { line: 1, column: 1 }));
    Err(LoadError::new(file, at, message))
}

/// The TLS files of `tls`.
pub(super) fn tls_paths(tls: Option<&RealtimeTls>) -> Vec<&Located<String>> {
    let Some(tls) = tls else {
        return Vec::new();
    };
    [
        &tls.ca_bundle,
        &tls.client_cert,
        &tls.client_key,
        &tls.client_pkcs12,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// One step of a scripted sequence.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawStep")]
pub enum Step {
    Send(Frame),
    WaitFor(Box<Checked<WaitFor>>),
    /// Milliseconds.
    Sleep(u64),
    Close(CloseStep),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    send: Option<Located<Frame>>,
    wait_for: Option<Located<Checked<WaitFor>>>,
    sleep: Option<Located<u64>>,
    close: Option<Located<CloseStep>>,
}

impl TryFrom<RawStep> for Step {
    type Error = String;

    fn try_from(raw: RawStep) -> Result<Self, String> {
        exactly_one(
            "a step",
            &[
                ("send", seen(&raw.send)),
                ("wait_for", seen(&raw.wait_for)),
                ("sleep", seen(&raw.sleep)),
                ("close", seen(&raw.close)),
            ],
        )?;
        let step = if let Some(frame) = raw.send {
            Self::Send(frame.into_inner())
        } else if let Some(wait) = raw.wait_for {
            Self::WaitFor(Box::new(wait.into_inner()))
        } else if let Some(ms) = raw.sleep {
            Self::Sleep(ms.into_inner())
        } else {
            let close = raw.close.expect("exactly one step key");
            Self::Close(close.into_inner().checked()?)
        };
        Ok(step)
    }
}

/// A frame to send.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawFrame")]
pub enum Frame {
    /// A text frame; interpolated.
    Text(String),
    /// A YAML/JSON value sent compactly as a text frame; string values
    /// are interpolated.
    Json(Value),
    Binary(BinarySource),
    /// A ping carrying base64-encoded application data.
    Ping(String),
}

impl Frame {
    /// The frame type as the timeline names it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Json(_) => "json",
            Self::Binary(_) => "binary",
            Self::Ping(_) => "ping",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrame {
    text: Option<Located<String>>,
    json: Option<Located<JsonValue>>,
    binary: Option<Located<BinarySource>>,
    ping: Option<Located<String>>,
}

impl TryFrom<RawFrame> for Frame {
    type Error = String;

    fn try_from(raw: RawFrame) -> Result<Self, String> {
        exactly_one(
            "send",
            &[
                ("text", seen(&raw.text)),
                ("json", seen(&raw.json)),
                ("binary", seen(&raw.binary)),
                ("ping", seen(&raw.ping)),
            ],
        )?;
        let frame = if let Some(text) = raw.text {
            Self::Text(text.into_inner())
        } else if let Some(json) = raw.json {
            Self::Json(json.into_inner().0)
        } else if let Some(binary) = raw.binary {
            Self::Binary(binary.into_inner())
        } else {
            Self::Ping(raw.ping.map(Located::into_inner).unwrap_or_default())
        };
        Ok(frame)
    }
}

/// The bytes of a binary frame: base64 text, or `{file: path}` (relative
/// to the request file, inside the workspace).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinarySource {
    Base64(String),
    File(Located<String>),
}

impl<'de> Deserialize<'de> for BinarySource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(BinaryVisitor)
    }
}

struct BinaryVisitor;

impl<'de> Visitor<'de> for BinaryVisitor {
    type Value = BinarySource;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("base64 text or `{file: path}`")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<BinarySource, E> {
        Ok(BinarySource::Base64(v.to_owned()))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BinarySource, A::Error> {
        let mut file = None;
        while let Some(key) = map.next_key::<String>()? {
            if key != "file" || file.is_some() {
                return Err(de::Error::custom(format!(
                    "binary accepts only `file`, found `{key}`"
                )));
            }
            file = Some(map.next_value::<Located<String>>()?);
        }
        file.map(BinarySource::File)
            .ok_or_else(|| de::Error::custom("binary needs `file` or base64 text"))
    }
}

/// `wait_for`: wait for a received message that matches, up to
/// `timeout_ms`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitFor {
    /// A JSONPath check on the message parsed as JSON.
    pub json: Option<Checked<JsonAssertion>>,
    /// A regular expression matched against the message text.
    pub regex: Option<String>,
    pub timeout_ms: Option<u64>,
}

impl Validate for WaitFor {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "wait_for",
            &[
                ("json", given(self.json.is_some())),
                ("regex", given(self.regex.is_some())),
            ],
        )
    }
}

impl WaitFor {
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
    }

    /// The matcher as error messages name it: `regex pong`,
    /// `json $.id equals 1`.
    pub fn describe(&self) -> String {
        if let Some(pattern) = &self.regex {
            return format!("regex {pattern}");
        }
        let Some(json) = &self.json else {
            return String::new();
        };
        let operand = |op: &str, v: &Value| format!("json {} {op} {v}", json.path);
        if let Some(v) = &json.equals {
            operand("equals", v)
        } else if let Some(pattern) = &json.matches {
            format!("json {} matches {pattern}", json.path)
        } else if json.exists == Some(false) {
            format!("json {} not exists", json.path)
        } else {
            format!("json {} exists", json.path)
        }
    }
}

/// `close`: a close frame with `code` (default 1000) and `reason`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseStep {
    pub code: Option<u16>,
    pub reason: Option<String>,
}

/// The longest close reason, in bytes: a control frame carries at most
/// 125 bytes, two of them the code (RFC 6455 5.5).
pub const MAX_CLOSE_REASON: usize = 123;

impl CloseStep {
    fn checked(self) -> Result<Self, String> {
        let reason = self.reason.as_deref().map_or(0, str::len);
        match self.code {
            Some(code) if !is_sendable_close_code(code) => Err(format!(
                "close code {code} cannot be sent (use 1000-1003, 1007-1014 or 3000-4999)"
            )),
            _ if reason > MAX_CLOSE_REASON => Err(format!(
                "close reason is {reason} bytes (at most {MAX_CLOSE_REASON})"
            )),
            _ => Ok(self),
        }
    }

    pub fn effective_code(&self) -> u16 {
        self.code.unwrap_or(NORMAL_CLOSE)
    }
}

/// Whether a client may put `code` in a close frame (RFC 6455 7.4).
fn is_sendable_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// `limits`: headless bounds, the message size limit and the timeline cap.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeLimits {
    pub max_messages: Option<u32>,
    pub duration_ms: Option<u64>,
    pub max_message_bytes: Option<u64>,
    pub max_timeline: Option<u32>,
}

impl Validate for RealtimeLimits {
    fn validate(&self) -> Result<(), String> {
        let zero = [
            ("max_messages", self.max_messages == Some(0)),
            ("duration_ms", self.duration_ms == Some(0)),
            ("max_message_bytes", self.max_message_bytes == Some(0)),
            ("max_timeline", self.max_timeline == Some(0)),
        ];
        match zero.iter().find(|(_, is_zero)| *is_zero) {
            Some((name, _)) => Err(format!("limits.{name} must be at least 1")),
            None => Ok(()),
        }
    }
}

impl RealtimeLimits {
    pub fn effective_max_messages(&self) -> u32 {
        self.max_messages.unwrap_or(DEFAULT_MAX_MESSAGES)
    }

    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms.unwrap_or(DEFAULT_DURATION_MS)
    }

    pub fn effective_max_message_bytes(&self) -> u64 {
        self.max_message_bytes.unwrap_or(DEFAULT_MAX_MESSAGE_BYTES)
    }

    pub fn effective_max_timeline(&self) -> u32 {
        self.max_timeline.unwrap_or(DEFAULT_MAX_TIMELINE)
    }
}

/// `tls`: the CA, client certificate and verification settings of
/// `http-requests`. `Debug` hides the PKCS#12 password.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTls {
    /// `false` accepts any certificate (`insecure`).
    pub verify: Option<bool>,
    pub ca_bundle: Option<Located<String>>,
    pub client_cert: Option<Located<String>>,
    pub client_key: Option<Located<String>>,
    pub client_pkcs12: Option<Located<String>>,
    pub client_pkcs12_password: Option<String>,
}

impl RealtimeTls {
    /// The settings, as a `kind: http` request has them.
    pub fn settings(&self) -> RequestSettings {
        RequestSettings {
            verify_tls: self.verify,
            ca_bundle: self.ca_bundle.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            client_pkcs12: self.client_pkcs12.clone(),
            client_pkcs12_password: self.client_pkcs12_password.clone(),
            ..RequestSettings::default()
        }
    }
}

impl fmt::Debug for RealtimeTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let password = self
            .client_pkcs12_password
            .as_ref()
            .map(|_| Hidden("client_pkcs12_password"));
        f.debug_struct("RealtimeTls")
            .field("verify", &self.verify)
            .field("ca_bundle", &self.ca_bundle)
            .field("client_cert", &self.client_cert)
            .field("client_key", &self.client_key)
            .field("client_pkcs12", &self.client_pkcs12)
            .field("client_pkcs12_password", &password)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "version: 1\nkind: ws\nname: feed\n";

    fn parse(text: &str) -> Result<WsRequest, LoadError> {
        WsRequest::parse(Path::new("f.rman.yaml"), text)
    }

    #[test]
    fn a_valid_file_loads_as_a_websocket_session() {
        let r = parse(&format!(
            "{HEAD}url: \"wss://{{{{host}}}}/feed\"\nsubprotocols: [v2.feed]\n"
        ))
        .unwrap();
        assert_eq!(r.url.get(), "wss://{{host}}/feed");
        assert_eq!(r.subprotocols.as_deref(), Some(&["v2.feed".to_owned()][..]));
        let limits = r.effective_limits();
        assert_eq!(limits.effective_max_messages(), 50);
        assert_eq!(limits.effective_duration_ms(), 30_000);
        assert_eq!(limits.effective_max_message_bytes(), 16 * 1024 * 1024);
        assert_eq!(limits.effective_max_timeline(), 10_000);
    }

    #[test]
    fn an_http_url_is_rejected_at_its_line() {
        let err = parse(&format!("{HEAD}url: https://example.com/feed\n")).unwrap_err();
        assert_eq!(err.message, "ws: url must use ws:// or wss://");
        assert_eq!(err.position.map(|p| p.line), Some(4));
        assert!(parse(&format!("{HEAD}url: '{{{{base}}}}/feed'\n")).is_ok());
        assert!(parse(&format!("{HEAD}url: WS://h/\n")).is_ok());
    }

    #[test]
    fn steps_load_in_order() {
        let text = format!(
            "{HEAD}url: ws://h/\nsteps:\n  - send: {{json: {{op: subscribe, channel: \"ticker\"}}}}\n  - send: {{text: hi}}\n  - send: {{binary: {{file: ./fixtures/blob.bin}}}}\n  - send: {{binary: AAE=}}\n  - send: {{ping: \"\"}}\n  - wait_for: {{json: {{path: \"$.id\", equals: 1}}}}\n  - wait_for: {{regex: pong, timeout_ms: 300}}\n  - sleep: 20\n  - close: {{code: 4000, reason: bye}}\n"
        );
        let r = parse(&text).unwrap();
        let steps = r.step_list();
        assert_eq!(steps.len(), 9);
        assert_eq!(
            steps[0],
            Step::Send(Frame::Json(
                serde_json::json!({"op": "subscribe", "channel": "ticker"})
            ))
        );
        assert_eq!(steps[1], Step::Send(Frame::Text("hi".into())));
        let Step::Send(Frame::Binary(BinarySource::File(path))) = &steps[2] else {
            panic!("a file frame: {:?}", steps[2]);
        };
        assert_eq!(path.get(), "./fixtures/blob.bin");
        assert_eq!(
            steps[3],
            Step::Send(Frame::Binary(BinarySource::Base64("AAE=".into())))
        );
        assert_eq!(steps[4], Step::Send(Frame::Ping(String::new())));
        let Step::WaitFor(wait) = &steps[5] else {
            panic!("wait_for");
        };
        assert_eq!(wait.describe(), "json $.id equals 1");
        assert_eq!(wait.effective_timeout_ms(), 5000);
        let Step::WaitFor(wait) = &steps[6] else {
            panic!("wait_for");
        };
        assert_eq!(wait.describe(), "regex pong");
        assert_eq!(wait.effective_timeout_ms(), 300);
        assert_eq!(steps[7], Step::Sleep(20));
        let Step::Close(close) = &steps[8] else {
            panic!("close");
        };
        assert_eq!(
            (close.effective_code(), close.reason.as_deref()),
            (4000, Some("bye"))
        );
        assert_eq!(r.file_paths().len(), 1);
    }

    #[test]
    fn invalid_steps_are_refused() {
        let long_reason = format!("- close: {{reason: {}}}", "r".repeat(124));
        let cases = [
            (
                "- {send: {text: a}, sleep: 1}",
                "a step needs exactly one of",
            ),
            ("- send: {text: a, json: 1}", "send needs exactly one of"),
            (
                "- wait_for: {timeout_ms: 5}",
                "wait_for needs exactly one of",
            ),
            ("- close: {code: 1006}", "close code 1006 cannot be sent"),
            (
                long_reason.as_str(),
                "close reason is 124 bytes (at most 123)",
            ),
            ("- send: {binary: {path: x}}", "binary accepts only `file`"),
            ("- jump: 1", "unknown field"),
        ];
        for (step, message) in cases {
            let err = parse(&format!("{HEAD}url: ws://h/\nsteps:\n  {step}\n")).unwrap_err();
            assert!(err.message.contains(message), "{step}: {}", err.message);
        }
    }

    #[test]
    fn zero_limits_are_refused() {
        let err = parse(&format!(
            "{HEAD}url: ws://h/\nlimits: {{max_messages: 0}}\n"
        ))
        .unwrap_err();
        assert!(
            err.message
                .contains("limits.max_messages must be at least 1"),
            "{}",
            err.message
        );
    }

    #[test]
    fn tls_settings_map_to_request_settings_and_hide_the_password() {
        let r = parse(&format!(
            "{HEAD}url: wss://h/\ntls: {{verify: false, ca_bundle: ca.pem, client_pkcs12: id.p12, client_pkcs12_password: pw-x}}\n"
        ))
        .unwrap();
        let settings = r.http_settings().unwrap();
        assert_eq!(settings.verify_tls, Some(false));
        assert_eq!(
            settings.ca_bundle.as_deref().map(String::as_str),
            Some("ca.pem")
        );
        assert_eq!(r.file_paths().len(), 2);
        let debug = format!("{:?}", r.tls);
        assert!(!debug.contains("pw-x"), "{debug}");
    }
}
