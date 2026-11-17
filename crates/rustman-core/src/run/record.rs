//! What a run produced, request by request. Every text field is already
//! redacted, so reporters and history can write records as they are.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::assertions::AssertionResult;
use crate::captures::CaptureResult;
use crate::redact::Redactor;
use crate::vars::{ResolvedBody, ResolvedRequest};

/// How one request of a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    /// Every assertion passed and every required capture matched.
    Passed,
    /// An assertion did not pass, or a required capture matched nothing.
    Failed,
    /// No response: unresolved variables, network, TLS, timeout, auth.
    Errored,
    /// Not executed; see [`SkipReason`].
    Skipped,
}

/// Why a request was not executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// An earlier request failed or errored under `--bail`.
    Bail,
    /// The run was interrupted (Ctrl+C) before or while it was sent.
    Interrupted,
    /// A request kind this build cannot execute (e.g. `graphql`, `mcp`).
    UnsupportedKind,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bail => "bail",
            Self::Interrupted => "interrupted",
            Self::UnsupportedKind => "unsupported kind",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderRecord {
    pub name: String,
    pub value: String,
}

/// The request as sent, redacted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SentRequest {
    pub method: String,
    /// The URL with its query parameters.
    pub url: String,
    pub headers: Vec<HeaderRecord>,
    /// A text body (JSON, text or form); `None` for no body, multipart or a file.
    pub body: Option<String>,
}

impl SentRequest {
    pub fn new(request: &ResolvedRequest, redactor: &Redactor) -> Self {
        let mut url = redactor.resolved(&request.url);
        for (i, pair) in request.query.iter().enumerate() {
            url.push(if i == 0 && !url.contains('?') {
                '?'
            } else {
                '&'
            });
            url.push_str(&redactor.resolved(&pair.name));
            url.push('=');
            url.push_str(&redactor.resolved(&pair.value));
        }
        let headers = request
            .headers
            .iter()
            .map(|p| HeaderRecord {
                name: p.name.text.clone(),
                value: redactor.resolved_header_value(&p.name.text, &p.value),
            })
            .collect();
        Self {
            method: request.method.clone(),
            url,
            headers,
            body: body_text(&request.body, redactor),
        }
    }
}

fn body_text(body: &ResolvedBody, redactor: &Redactor) -> Option<String> {
    let text = match body {
        ResolvedBody::Json(value) => value.to_string(),
        ResolvedBody::JsonText(text) | ResolvedBody::Text(text) => {
            return Some(redactor.resolved(text));
        }
        ResolvedBody::Form(fields) => fields
            .iter()
            .map(|f| format!("{}={}", f.name.text, f.value.text))
            .collect::<Vec<_>>()
            .join("&"),
        ResolvedBody::None | ResolvedBody::Multipart(_) | ResolvedBody::File(_) => return None,
    };
    Some(redactor.text(&text).into_owned())
}

/// The response, redacted.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResponseRecord {
    pub status: u16,
    pub http_version: String,
    pub headers: Vec<HeaderRecord>,
    /// The decoded text body; `None` when it is binary.
    pub body: Option<String>,
    /// The size of the whole decoded body.
    pub body_bytes: u64,
    /// `body` holds only the start of a body spilled to disk.
    pub body_truncated: bool,
    pub binary: bool,
}

/// Phase durations in milliseconds; `None` for a phase that did not happen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TimingsRecord {
    pub dns_ms: Option<f64>,
    pub connect_ms: Option<f64>,
    pub tls_ms: Option<f64>,
    pub ttfb_ms: f64,
    pub download_ms: f64,
    pub total_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RedirectRecord {
    pub status: u16,
    pub url: String,
}

/// One exchange: request, response and how it went.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExchangeRecord {
    pub request: SentRequest,
    pub response: ResponseRecord,
    pub timings: TimingsRecord,
    pub redirects: Vec<RedirectRecord>,
    /// Sent over TLS with certificate verification off.
    pub insecure: bool,
}

/// One request of a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RequestRecord {
    /// The request id: its path without `.rman.yaml`.
    pub id: String,
    pub name: String,
    /// The request file, relative to the workspace root, with `/`.
    pub file: String,
    /// 1-based, for `--iterations`.
    pub iteration: u32,
    pub status: RequestStatus,
    pub skip_reason: Option<SkipReason>,
    /// Why the request errored, or why a required capture failed it.
    pub error: Option<String>,
    pub exchange: Option<ExchangeRecord>,
    pub assertions: Vec<AssertionResult>,
    pub captures: Vec<CaptureResult>,
    pub warnings: Vec<String>,
    pub duration_ms: f64,
}

impl RequestRecord {
    /// A record with no outcome yet, for `id`, `name` and `file`.
    pub fn new(id: String, name: String, file: String, iteration: u32) -> Self {
        Self {
            id,
            name,
            file,
            iteration,
            status: RequestStatus::Skipped,
            skip_reason: None,
            error: None,
            exchange: None,
            assertions: Vec::new(),
            captures: Vec::new(),
            warnings: Vec::new(),
            duration_ms: 0.0,
        }
    }

    pub fn skipped(mut self, reason: SkipReason) -> Self {
        self.status = RequestStatus::Skipped;
        self.skip_reason = Some(reason);
        self
    }

    pub fn errored(mut self, message: String) -> Self {
        self.status = RequestStatus::Errored;
        self.error = Some(message);
        self
    }
}

/// Counts over every request of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errored: usize,
    pub skipped: usize,
    pub duration_ms: f64,
    /// The run was stopped by Ctrl+C; the records are partial.
    pub interrupted: bool,
}

impl Summary {
    pub fn of(records: &[RequestRecord], duration_ms: f64, interrupted: bool) -> Self {
        let count = |status| records.iter().filter(|r| r.status == status).count();
        Self {
            total: records.len(),
            passed: count(RequestStatus::Passed),
            failed: count(RequestStatus::Failed),
            errored: count(RequestStatus::Errored),
            skipped: count(RequestStatus::Skipped),
            duration_ms,
            interrupted,
        }
    }
}

/// A whole run, in collection order (iteration by iteration).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunRecord {
    /// RFC 3339 UTC, e.g. `2026-11-17T09:30:00.123Z`.
    pub started_at: String,
    /// The active environment, if any.
    pub environment: Option<String>,
    pub requests: Vec<RequestRecord>,
    pub summary: Summary,
}

/// The current time, RFC 3339 UTC with milliseconds.
pub fn now_rfc3339() -> String {
    crate::vars::dynamic::iso_now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;
    use crate::vars::{Resolved, ResolvedPair, SecretSpan};

    fn record(status: RequestStatus) -> RequestRecord {
        let mut r = RequestRecord::new("a".into(), "A".into(), "a.rman.yaml".into(), 1);
        r.status = status;
        r
    }

    #[test]
    fn summary_counts_every_status() {
        let records = [
            record(RequestStatus::Passed),
            record(RequestStatus::Failed),
            record(RequestStatus::Errored),
            record(RequestStatus::Skipped).skipped(SkipReason::Bail),
            record(RequestStatus::Passed),
        ];
        let summary = Summary::of(&records, 12.5, false);
        assert_eq!((summary.total, summary.passed, summary.failed), (5, 2, 1));
        assert_eq!((summary.errored, summary.skipped), (1, 1));
    }

    #[test]
    fn sent_requests_are_redacted() {
        let token = Secret::new("token", "s3cr3t".to_owned());
        let request = ResolvedRequest {
            method: "POST".into(),
            url: Resolved::literal("http://h/x"),
            query: vec![ResolvedPair {
                name: Resolved::literal("key"),
                value: Resolved {
                    text: "s3cr3t".into(),
                    secret_spans: vec![SecretSpan {
                        name: "token".into(),
                        range: 0..6,
                    }],
                },
            }],
            headers: vec![ResolvedPair {
                name: Resolved::literal("Authorization"),
                value: Resolved::literal("Bearer literal"),
            }],
            body: ResolvedBody::Text(Resolved::literal("echo s3cr3t")),
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: vec![token],
        };
        let sent = SentRequest::new(&request, &Redactor::for_request(&request));
        assert_eq!(sent.url, "http://h/x?key=«redacted:token»");
        assert_eq!(sent.headers[0].value, "«redacted:Authorization»");
        assert_eq!(sent.body.as_deref(), Some("echo «redacted:token»"));
    }

    #[test]
    fn skip_reasons_print_as_in_reports() {
        assert_eq!(SkipReason::Bail.to_string(), "bail");
        assert_eq!(SkipReason::Interrupted.to_string(), "interrupted");
        assert_eq!(
            serde_json::to_value(SkipReason::UnsupportedKind).unwrap(),
            serde_json::json!("unsupported_kind")
        );
    }
}
