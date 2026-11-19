//! The request log (spec: Request log, History integration): one entry per
//! request with its time, method, path, the operation or override that
//! answered, the status, where the response came from, validation errors
//! and duration. Entries are redacted when they are made, so every sink
//! (JSON lines, the human line, the TUI, history) shows the same text.

use http::HeaderMap;
use rustman_core::Redactor;
use rustman_core::history::{ENTRY_SCHEMA, Entry, new_ulid};
use rustman_core::redact::MIN_HEADER_CREDENTIAL;
use rustman_core::run::{HeaderRecord, SentRequest};
use rustman_core::secret::placeholder;
use serde::Serialize;
use serde_json::json;

use crate::credentials::Credentials;
use crate::respond::Source;

/// What `request` is for a history entry of a served exchange.
pub const HISTORY_REQUEST: &str = "mock";

/// One served request, redacted.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogEntry {
    /// RFC 3339 UTC with milliseconds.
    pub time: String,
    pub method: String,
    /// The path with its query, as sent.
    pub path: String,
    /// The `operationId` (else `METHOD /template`) of the matched operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// The name of the override that answered.
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_name: Option<String>,
    /// `None` when a fault dropped the connection.
    pub status: Option<u16>,
    pub source: Source,
    /// Validation errors (`location pointer: message`) and other notes,
    /// such as unresolved template references.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    pub duration_ms: f64,
    /// Request headers; credentials always show as placeholders.
    pub headers: Vec<HeaderRecord>,
    /// The request body when it is text, for history only.
    #[serde(skip)]
    pub body: Option<String>,
}

/// What the server reports besides responses.
#[derive(Clone, Debug, PartialEq)]
pub enum MockEvent {
    Request(Box<LogEntry>),
    /// The document and overrides were reloaded.
    Reloaded {
        operations: usize,
        overrides: usize,
    },
    /// A changed document or mock file did not load; the previous version
    /// is still served. The message names the file and line.
    ReloadFailed(String),
    /// A proxied exchange was saved as a request file (its path).
    Recorded(String),
    /// A proxied exchange could not be saved.
    RecordFailed(String),
}

impl MockEvent {
    /// The line printed for an event that is not a request.
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Request(_) => None,
            Self::Reloaded {
                operations,
                overrides,
            } => Some(format!(
                "mock: reloaded: {operations} operations, {overrides} overrides"
            )),
            Self::ReloadFailed(message) => Some(format!(
                "mock: reload failed, still serving the previous version: {message}"
            )),
            Self::Recorded(path) => Some(format!("mock: recorded {path}")),
            Self::RecordFailed(message) => Some(format!("mock: cannot record: {message}")),
        }
    }
}

/// The request half of a log entry, before the response is known.
pub struct Received<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

impl Received<'_> {
    /// A log entry for this request, everything redacted with `redactor`,
    /// and the values of the headers and query parameters `credentials`
    /// names replaced by placeholders (and redacted wherever they appear).
    pub fn entry(&self, redactor: &Redactor, credentials: &Credentials) -> LogEntry {
        let mut redactor = redactor.clone();
        for (name, value) in self.credential_values(credentials) {
            if value.chars().count() >= MIN_HEADER_CREDENTIAL {
                redactor.add_value(&name, &value);
            }
        }
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| HeaderRecord {
                name: name.as_str().to_owned(),
                value: header_value(&redactor, credentials, name.as_str(), value.as_bytes()),
            })
            .collect();
        let body = (!self.body.is_empty())
            .then(|| std::str::from_utf8(self.body).ok())
            .flatten()
            .map(|text| redactor.text(text).into_owned());
        let path = redact_query(self.path_and_query, credentials);
        LogEntry {
            time: rustman_core::run::now_rfc3339(),
            method: self.method.to_owned(),
            path: redactor.text(&path).into_owned(),
            operation: None,
            override_name: None,
            status: None,
            source: Source::Error,
            errors: Vec::new(),
            duration_ms: 0.0,
            headers,
            body,
        }
    }
}

impl Received<'_> {
    /// The credential header and query values this request carries.
    fn credential_values(&self, credentials: &Credentials) -> Vec<(String, String)> {
        let headers = self
            .headers
            .iter()
            .filter(|(name, _)| credentials.is_header(name.as_str()))
            .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?)))
            .map(|(name, value)| (name, value.trim().to_owned()));
        let mut values: Vec<(String, String)> = headers.collect();
        values.extend(credentials.query_values(self.path_and_query));
        values
    }
}

/// A header value as it may be logged: credential headers whole as
/// `«redacted:<name>»`, others through `redactor`.
fn header_value(
    redactor: &Redactor,
    credentials: &Credentials,
    name: &str,
    value: &[u8],
) -> String {
    if credentials.is_header(name) {
        return placeholder(name);
    }
    redactor.header_value(name, &String::from_utf8_lossy(value))
}

/// `path_and_query` with the value of every credential query parameter
/// replaced by `«redacted:<name>»`.
fn redact_query(path_and_query: &str, credentials: &Credentials) -> String {
    let Some((path, query)) = path_and_query.split_once('?') else {
        return path_and_query.to_owned();
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if !value.is_empty() && is_credential_param(credentials, name) => {
                format!("{name}={}", placeholder(name))
            }
            _ => pair.to_owned(),
        })
        .collect();
    format!("{path}?{}", pairs.join("&"))
}

fn is_credential_param(credentials: &Credentials, raw_name: &str) -> bool {
    let name = percent_encoding::percent_decode_str(raw_name).decode_utf8_lossy();
    credentials.is_query(&name)
}

impl LogEntry {
    /// One JSON object, for `--jsonl`.
    pub fn json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// `GET /pets/1 -> 200 example (getPet) 1.2 ms`, plus the errors.
    pub fn human_line(&self) -> String {
        let status = self
            .status
            .map_or_else(|| "dropped".to_owned(), |s| s.to_string());
        let matched = self
            .override_name
            .as_ref()
            .map(|o| format!(" (override {o})"))
            .or_else(|| self.operation.as_ref().map(|o| format!(" ({o})")))
            .unwrap_or_default();
        let mut line = format!(
            "{} {} {} -> {status} {}{matched} {:.1} ms",
            self.time,
            self.method,
            self.path,
            self.source.as_str(),
            self.duration_ms
        );
        if !self.errors.is_empty() {
            line.push_str(&format!(" [{}]", self.errors.join("; ")));
        }
        line
    }

    /// A `request-history` entry tagged `mock`, for a mock served at
    /// `base_url` (`http://127.0.0.1:4010`). It cannot be replayed.
    pub fn history_entry(&self, base_url: &str) -> Entry {
        let record = json!({
            "mock": {
                "source": self.source,
                "operation": self.operation,
                "override": self.override_name,
                "errors": self.errors,
            }
        });
        Entry {
            schema: ENTRY_SCHEMA.to_owned(),
            id: new_ulid(),
            timestamp: self.time.clone(),
            request: HISTORY_REQUEST.to_owned(),
            environment: None,
            sent: SentRequest {
                method: self.method.clone(),
                url: format!("{base_url}{}", self.path),
                headers: self.headers.clone(),
                body: self.body.clone(),
            },
            status: self.status,
            total_ms: Some(self.duration_ms),
            error: None,
            spill: None,
            record,
            replay: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use rustman_core::Secret;

    use super::*;

    fn entry(redactor: &Redactor) -> LogEntry {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc"));
        headers.insert("x-trace", HeaderValue::from_static("canary-5d1e"));
        let received = Received {
            method: "GET",
            path_and_query: "/pets/1?k=canary-5d1e",
            headers: &headers,
            body: b"{\"k\":\"canary-5d1e\"}",
        };
        let mut entry = received.entry(redactor, &Credentials::default());
        entry.status = Some(200);
        entry.source = Source::Example;
        entry.operation = Some("getPet".into());
        entry
    }

    fn redactor() -> Redactor {
        Redactor::new().with(&Secret::new("key", "canary-5d1e".to_owned()))
    }

    #[test]
    fn json_lines_carry_status_source_and_redacted_headers() {
        let line = entry(&redactor()).json_line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["status"], 200);
        assert_eq!(value["source"], "example");
        assert_eq!(value["operation"], "getPet");
        assert_eq!(value["headers"][0]["value"], "«redacted:Authorization»");
        assert!(!line.contains("abc"), "{line}");
        assert!(!line.contains("canary-5d1e"), "{line}");
        assert!(value.get("body").is_none());
    }

    #[test]
    fn the_human_line_names_status_source_and_match() {
        let mut e = entry(&Redactor::new());
        e.errors = vec!["query /q: required".into()];
        let line = e.human_line();
        assert!(
            line.contains("GET /pets/1?k=canary-5d1e -> 200 example (getPet)"),
            "{line}"
        );
        assert!(line.ends_with("ms [query /q: required]"), "{line}");
        e.status = None;
        e.override_name = Some("slow".into());
        assert!(
            e.human_line()
                .contains("-> dropped example (override slow)")
        );
    }

    #[test]
    fn history_entries_are_tagged_mock_and_redacted() {
        let e = entry(&redactor());
        let history = e.history_entry("http://127.0.0.1:4010");
        assert_eq!(history.request, "mock");
        assert_eq!(history.status, Some(200));
        assert!(history.sent.url.starts_with("http://127.0.0.1:4010/pets/1"));
        let text = serde_json::to_string(&history).unwrap();
        assert!(!text.contains("canary-5d1e"), "{text}");
        assert!(!text.contains("Bearer abc"), "{text}");
        assert!(history.replay.is_none());
    }

    #[test]
    fn credential_headers_and_query_parameters_are_redacted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_static("canary-apikey-123456"),
        );
        headers.insert("x-partner", HeaderValue::from_static("canary-partner-77"));
        headers.insert("accept", HeaderValue::from_static("application/json"));
        let received = Received {
            method: "GET",
            path_and_query: "/node?limit=2&api_key=qcanary99&key=k1&sig=canary-sig-42",
            headers: &headers,
            body: b"{\"echo\":\"canary-partner-77\"}",
        };
        let model = rustman_explore::openapi::lower::lower(&json!({
            "openapi": "3.1.0",
            "info": {"title": "t", "version": "1"},
            "paths": {},
            "components": {"securitySchemes": {
                "partner": {"type": "apiKey", "in": "header", "name": "X-Partner"},
                "sig": {"type": "apiKey", "in": "query", "name": "sig"},
            }},
        }))
        .unwrap();
        let entry = received.entry(&Redactor::new(), &Credentials::new(&model));
        let line = entry.json_line();
        let history = serde_json::to_string(&entry.history_entry("http://127.0.0.1:1")).unwrap();
        for text in [&line, &entry.human_line(), &history] {
            for canary in [
                "canary-apikey-123456",
                "canary-partner-77",
                "qcanary99",
                "k1",
                "canary-sig-42",
            ] {
                assert!(!text.contains(canary), "{canary} in {text}");
            }
        }
        assert_eq!(entry.headers[0].value, "«redacted:x-api-key»");
        assert_eq!(entry.headers[2].value, "application/json");
        assert_eq!(
            entry.path,
            "/node?limit=2&api_key=«redacted:api_key»&key=«redacted:key»&sig=«redacted:sig»"
        );
        assert_eq!(
            entry.body.as_deref(),
            Some("{\"echo\":\"«redacted:x-partner»\"}")
        );
    }

    #[test]
    fn notices_describe_reloads_and_records() {
        let reloaded = MockEvent::Reloaded {
            operations: 3,
            overrides: 1,
        };
        assert_eq!(
            reloaded.notice().unwrap(),
            "mock: reloaded: 3 operations, 1 overrides"
        );
        let failed = MockEvent::ReloadFailed("api.yaml: line 3".into());
        assert!(failed.notice().unwrap().contains("api.yaml: line 3"));
        assert!(
            MockEvent::Request(Box::new(entry(&Redactor::new())))
                .notice()
                .is_none()
        );
    }
}
