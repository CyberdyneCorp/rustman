//! One recorded exchange, as stored in `.rustman/history/<ulid>.json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::replay::ReplayRequest;
use super::ulid::new_ulid;
use crate::run::{RequestRecord, SentRequest, now_rfc3339};

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

    /// The redacted response body text (or its start, when spilled).
    pub fn response_body(&self) -> Option<&str> {
        self.record
            .pointer("/exchange/response/body")
            .and_then(Value::as_str)
    }
}
