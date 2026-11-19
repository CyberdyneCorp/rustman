//! One recorded exchange, as stored in `.rustman/history/<ulid>.json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::replay::ReplayRequest;
use super::ulid::new_ulid;
use crate::run::{RequestRecord, SentRequest, now_rfc3339};
use crate::trace::TraceRecord;

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
    /// A new entry with a fresh id and the current time.
    pub fn new(new: NewEntry<'_>) -> Self {
        let exchange = new.record.exchange.as_ref();
        Self {
            schema: ENTRY_SCHEMA.to_owned(),
            id: new_ulid(),
            timestamp: now_rfc3339(),
            request: new.request.unwrap_or(AD_HOC_REQUEST).to_owned(),
            environment: new.environment.map(str::to_owned),
            sent: exchange.map_or(new.sent, |e| e.request.clone()),
            status: exchange.map(|e| e.response.status),
            total_ms: exchange.map(|e| e.timings.total_ms),
            error: new.record.error.clone(),
            spill: new.spill,
            record: serde_json::to_value(new.record).unwrap_or(Value::Null),
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

#[cfg(test)]
mod tests {
    use super::*;

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
