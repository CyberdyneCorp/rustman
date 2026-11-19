//! What an exchange records about its trace: the ids sent, the backend's
//! own trace id from `traceresponse`, the `tracestate` and `baggage` sent
//! (redacted) and the link.

use serde::{Deserialize, Serialize};

use super::link::{LinkIds, TraceLink};
use super::{TraceContext, w3c};
use crate::redact::Redactor;

/// The W3C response header naming the trace the server recorded.
pub const TRACERESPONSE: &str = "traceresponse";

/// The trace context a request carried, with the `tracestate` and
/// `baggage` values it sent (unredacted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentTrace {
    pub context: TraceContext,
    pub tracestate: Option<String>,
    pub baggage: Option<String>,
}

/// The `trace` object of history entries and report requests. Every field
/// but the ids is optional, so it can only grow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRecord {
    pub trace_id: String,
    pub span_id: String,
    pub sampled: bool,
    /// The trace id of the response's `traceresponse`, when it differs
    /// from `trace_id` (the backend ignored the incoming context).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baggage: Option<String>,
    /// Unix milliseconds of the exchange's start and end.
    #[serde(default)]
    pub start_ms: u64,
    #[serde(default)]
    pub end_ms: u64,
    /// The trace link, when a template is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl TraceRecord {
    /// The record of `sent`, whose response carried `traceresponse`, over
    /// `start_ms..end_ms`. `tracestate` and `baggage` values are redacted.
    pub fn new(
        sent: &SentTrace,
        traceresponse: Option<&str>,
        (start_ms, end_ms): (u64, u64),
        redactor: &Redactor,
    ) -> Self {
        let context = &sent.context;
        let redacted =
            |value: &Option<String>| value.as_deref().map(|v| redactor.text(v).into_owned());
        Self {
            trace_id: context.trace_id.to_string(),
            span_id: context.span_id.to_string(),
            sampled: context.sampled,
            backend_trace_id: traceresponse
                .and_then(w3c::parse_traceparent)
                .map(|backend| backend.trace_id)
                .filter(|backend| *backend != context.trace_id)
                .map(|backend| backend.to_string()),
            tracestate: redacted(&sent.tracestate),
            baggage: redacted(&sent.baggage),
            start_ms,
            end_ms,
            link: None,
        }
    }

    /// The trace a link opens: the backend's when it restarted the trace.
    pub fn link_trace_id(&self) -> &str {
        self.backend_trace_id.as_deref().unwrap_or(&self.trace_id)
    }

    /// `link` filled with this record's ids and window.
    pub fn render(&self, link: &TraceLink) -> String {
        link.render(&LinkIds {
            trace_id: self.link_trace_id(),
            span_id: &self.span_id,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{SpanId, TraceId};

    const SENT: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const OTHER: &str = "0af7651916cd43dd8448eb211c80319c";

    fn sent(baggage: Option<&str>) -> SentTrace {
        SentTrace {
            context: TraceContext {
                trace_id: TraceId::from_hex(SENT).unwrap(),
                span_id: SpanId::from_hex("00f067aa0ba902b7").unwrap(),
                sampled: true,
            },
            tracestate: Some("rojo=00f067aa0ba902b7".into()),
            baggage: baggage.map(str::to_owned),
        }
    }

    #[test]
    fn a_different_traceresponse_becomes_the_link_id() {
        let response = format!("00-{OTHER}-b7ad6b7169203331-01");
        let record = TraceRecord::new(&sent(None), Some(&response), (1, 2), &Redactor::new());
        assert_eq!(record.trace_id, SENT);
        assert_eq!(record.backend_trace_id.as_deref(), Some(OTHER));
        assert_eq!(record.link_trace_id(), OTHER);
        let link = TraceLink::new("http://j/trace/{trace_id}?s={start_ms}");
        assert_eq!(record.render(&link), format!("http://j/trace/{OTHER}?s=1"));
    }

    #[test]
    fn a_matching_or_malformed_traceresponse_is_not_a_backend_id() {
        let same = format!("00-{SENT}-b7ad6b7169203331-01");
        for response in [Some(same.as_str()), Some("00-garbage"), None] {
            let record = TraceRecord::new(&sent(None), response, (1, 2), &Redactor::new());
            assert_eq!(record.backend_trace_id, None, "{response:?}");
            assert_eq!(record.link_trace_id(), SENT);
        }
    }

    #[test]
    fn secret_baggage_values_are_redacted() {
        let mut redactor = Redactor::new();
        redactor.add_value("api_key", "s3cr3t-canary");
        let record = TraceRecord::new(
            &sent(Some("tenant=acme,api_key=s3cr3t-canary")),
            None,
            (1, 2),
            &redactor,
        );
        assert_eq!(
            record.baggage.as_deref(),
            Some("tenant=acme,api_key=«redacted:api_key»")
        );
        assert_eq!(record.trace_id, SENT, "ids are not secrets");
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("s3cr3t-canary"), "{json}");
        let back: TraceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn old_records_with_only_ids_load() {
        let record: TraceRecord = serde_json::from_str(&format!(
            r#"{{"trace_id":"{SENT}","span_id":"00f067aa0ba902b7","sampled":true}}"#
        ))
        .unwrap();
        assert_eq!((record.start_ms, record.link), (0, None));
    }
}
