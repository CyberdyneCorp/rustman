//! One recorded exchange, as stored in `.rustman/history/<ulid>.json`.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::replay::ReplayRequest;
use super::ulid::new_ulid;
use crate::run::{RequestRecord, SentRequest, now_rfc3339};
use crate::trace::TraceRecord;
use crate::url_secrets::{USERINFO_MARK, hide_url_userinfo, hide_userinfo, url_userinfos};

/// The `schema` of every stored entry.
pub const ENTRY_SCHEMA: &str = "rustman.history/v1";

/// What `request` is for an ad-hoc request.
pub const AD_HOC_REQUEST: &str = "adhoc";

/// A recorded exchange. Every text field is redacted; `replay` holds
/// placeholders where secrets were.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub schema: String,
    /// A ULID: entries sort by time.
    pub id: String,
    /// RFC 3339 UTC.
    pub timestamp: String,
    /// The request file relative to the workspace root (with `/`), or `adhoc`.
    pub request: String,
    pub environment: Option<String>,
    /// The request as sent.
    pub sent: SentRequest,
    /// The response status; `None` when no response arrived.
    pub status: Option<u16>,
    pub total_ms: Option<f64>,
    /// Why no response arrived, or why a required capture failed.
    pub error: Option<String>,
    /// The file name, under `.rustman/bodies/`, of a body too large to keep
    /// in memory; the record holds only its start.
    pub spill: Option<String>,
    /// The request record: response, timings, assertions and captures.
    pub record: Value,
    /// What `rustman history replay` re-sends.
    pub replay: Option<ReplayRequest>,
}

/// What an exchange is recorded from.
pub struct NewEntry<'a> {
    pub record: &'a RequestRecord,
    /// The request as sent, when the record has no exchange (no response).
    pub sent: SentRequest,
    /// The request file, `None` for an ad-hoc request.
    pub request: Option<&'a str>,
    pub environment: Option<&'a str>,
    pub spill: Option<String>,
    pub replay: Option<ReplayRequest>,
}

impl Entry {
    /// A new entry with a fresh id and the current time. The userinfo of
    /// every stored URL (`user:password@`) is replaced by a marker: a
    /// credential typed into a URL is no known secret, so the redactor
    /// keeps it.
    pub fn new(new: NewEntry<'_>) -> Self {
        let exchange = new.record.exchange.as_ref();
        let mut sent = exchange.map_or(new.sent, |e| e.request.clone());
        let mut record = serde_json::to_value(new.record).unwrap_or(Value::Null);
        let hider = UserinfoHider::new(&sent.url, &record);
        sent.url = hide_url_userinfo(&sent.url).into_owned();
        hider.hide_record(&mut record);
        Self {
            schema: ENTRY_SCHEMA.to_owned(),
            id: new_ulid(),
            timestamp: now_rfc3339(),
            request: new.request.unwrap_or(AD_HOC_REQUEST).to_owned(),
            environment: new.environment.map(str::to_owned),
            sent,
            status: exchange.map(|e| e.response.status),
            total_ms: exchange.map(|e| e.timings.total_ms),
            error: new.record.error.as_deref().map(|e| hider.text(e)),
            spill: new.spill,
            record,
            replay: new.replay,
        }
    }

    /// The trace context the exchange carried; `None` for entries recorded
    /// without one, or before trace propagation existed.
    pub fn trace(&self) -> Option<TraceRecord> {
        let trace = self.record.get("trace")?;
        serde_json::from_value(trace.clone()).ok()
    }

    /// The redacted response body text (or its start, when spilled).
    pub fn response_body(&self) -> Option<&str> {
        self.record
            .pointer("/exchange/response/body")
            .and_then(Value::as_str)
    }
}

/// Hides the userinfo of the URLs an entry stores: the request's and each
/// redirect's, read as URLs, and any URL in its error and warnings.
struct UserinfoHider {
    /// `://userinfo@` of the stored URLs, longest first: replaced literally
    /// in free text, whatever the password holds (an `invalid URL` error
    /// quotes a password with a space or a quote whole).
    known: Vec<String>,
}

impl UserinfoHider {
    fn new(sent_url: &str, record: &Value) -> Self {
        let mut known: Vec<String> = std::iter::once(sent_url)
            .chain(record_urls(record))
            .flat_map(|url| {
                url_userinfos(url)
                    .into_iter()
                    .map(move |r| format!("://{}@", &url[r]))
            })
            .collect();
        known.sort_by_key(|k| std::cmp::Reverse(k.len()));
        known.dedup();
        Self { known }
    }

    /// Free text with the userinfo of every URL in it hidden.
    fn text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for info in &self.known {
            if text.contains(info.as_str()) {
                text = text.replace(info.as_str(), &format!("://{USERINFO_MARK}@"));
            }
        }
        hide_userinfo(&text).into_owned()
    }

    fn hide_record(&self, record: &mut Value) {
        let url = |value: &mut Value| {
            if let Value::String(text) = value
                && let Cow::Owned(hidden) = hide_url_userinfo(text)
            {
                *text = hidden;
            }
        };
        let text = |value: &mut Value| {
            if let Value::String(text) = value {
                *text = self.text(text);
            }
        };
        if let Some(value) = record.pointer_mut("/exchange/request/url") {
            url(value);
        }
        if let Some(Value::Array(redirects)) = record.pointer_mut("/exchange/redirects") {
            redirects
                .iter_mut()
                .filter_map(|r| r.get_mut("url"))
                .for_each(url);
        }
        if let Some(error) = record.get_mut("error") {
            text(error);
        }
        if let Some(Value::Array(warnings)) = record.get_mut("warnings") {
            warnings.iter_mut().for_each(text);
        }
    }
}

/// The URLs `record` stores: the request's and each redirect's.
fn record_urls(record: &Value) -> impl Iterator<Item = &str> {
    let request = record
        .pointer("/exchange/request/url")
        .and_then(Value::as_str);
    let redirects = record
        .pointer("/exchange/redirects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("url").and_then(Value::as_str));
    request.into_iter().chain(redirects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_urls_keep_no_userinfo() {
        let mut record = RequestRecord::new("r".into(), "r".into(), "r.rman.yaml".into());
        record.error = Some("connect to https://alice:pw1@h.example/x failed".into());
        record.warnings = vec!["via https://tok@h.example".into()];
        let sent = SentRequest {
            method: "GET".into(),
            url: "https://alice:pw1@h.example/x?q=1".into(),
            headers: Vec::new(),
            body: None,
        };
        let entry = Entry::new(NewEntry {
            record: &record,
            sent,
            request: None,
            environment: None,
            spill: None,
            replay: None,
        });
        assert_eq!(
            entry.sent.url,
            "https://«redacted:userinfo»@h.example/x?q=1"
        );
        assert_eq!(
            entry.error.as_deref(),
            Some("connect to https://«redacted:userinfo»@h.example/x failed")
        );
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("pw1") && !json.contains("tok@"), "{json}");
    }

    /// A password holding `)`, `'`, a quote or a space (valid in userinfo,
    /// or quoted whole by an `invalid URL` error) is hidden whole, in the
    /// URL fields and in the error.
    #[test]
    fn passwords_with_sub_delims_or_spaces_are_hidden() {
        for (password, error) in [
            ("Pa)ss9", None),
            ("pa'ss", None),
            (
                "pa ss",
                Some("invalid URL 'https://bob:pa ss@api.example/x'"),
            ),
            (
                "pa\"s>s",
                Some("invalid URL 'https://bob:pa\"s>s@api.example/x'"),
            ),
        ] {
            let mut record = RequestRecord::new("r".into(), "r".into(), "r.rman.yaml".into());
            record.error = error.map(str::to_owned);
            let sent = SentRequest {
                method: "GET".into(),
                url: format!("https://bob:{password}@api.example/x"),
                headers: Vec::new(),
                body: None,
            };
            let entry = Entry::new(NewEntry {
                record: &record,
                sent,
                request: None,
                environment: None,
                spill: None,
                replay: None,
            });
            assert_eq!(
                entry.sent.url, "https://«redacted:userinfo»@api.example/x",
                "{password}"
            );
            if error.is_some() {
                assert_eq!(
                    entry.error.as_deref(),
                    Some("invalid URL 'https://«redacted:userinfo»@api.example/x'")
                );
            }
            let json = serde_json::to_string(&entry).unwrap();
            assert!(!json.contains("bob:"), "{json}");
        }
    }

    fn entry(record: Value) -> Entry {
        let json = serde_json::json!({
            "schema": ENTRY_SCHEMA,
            "id": "01JDQ3XG000000000000000000",
            "timestamp": "2026-11-17T09:30:00.123Z",
            "request": AD_HOC_REQUEST,
            "environment": null,
            "sent": {"method": "GET", "url": "http://x/", "headers": [], "body": null},
            "status": 200,
            "total_ms": 4.0,
            "error": null,
            "spill": null,
            "record": record,
            "replay": null,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn old_entries_load_without_a_trace() {
        let old = entry(serde_json::json!({"id": "a", "status": "passed"}));
        assert_eq!(old.trace(), None);
        let traced = entry(serde_json::json!({
            "trace": {
                "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
                "span_id": "00f067aa0ba902b7",
                "sampled": true
            }
        }));
        let trace = traced.trace().unwrap();
        assert_eq!(trace.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(trace.link, None);
    }
}
