//! The exchange of one gRPC call (design D7): its call type, status,
//! response headers and trailers, received messages, the message timeline
//! of streaming calls, and timings.

use std::time::Duration;

use std::borrow::Cow;

use rustman_core::Redactor;
use rustman_core::assertions::{GrpcView, ResponseView};
use rustman_core::run::redact_value;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use crate::browse::CallType;
use crate::metadata::MetadataEntry;
use crate::status::{DEADLINE_EXCEEDED, GrpcStatus};

/// The direction of a timeline entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Sent,
    Received,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Received => "received",
        }
    }
}

/// One message of a streaming call.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub direction: Direction,
    /// Since the call started.
    #[serde(rename = "at_ms", serialize_with = "millis")]
    pub at: Duration,
    /// The encoded message size.
    pub size_bytes: usize,
    pub message: Value,
}

/// Why a headless stream stopped before the server ended it (spec
/// "Headless stream bounds").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStop {
    /// `stream.max_messages` messages were received.
    MaxMessages(u32),
    /// `stream.duration_ms` elapsed.
    Duration(u64),
}

impl StreamStop {
    /// The message of the `CANCELLED` status a stopped stream ends with.
    pub fn message(self) -> String {
        match self {
            Self::MaxMessages(n) => {
                format!("stopped by rustman after {n} messages (stream.max_messages)")
            }
            Self::Duration(ms) => {
                format!("stopped by rustman after {ms} ms (stream.duration_ms)")
            }
        }
    }
}

/// How long the call took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GrpcTimings {
    /// The TCP connect.
    #[serde(rename = "connect_ms", serialize_with = "optional_millis")]
    pub connect: Option<Duration>,
    /// The TLS handshake.
    #[serde(rename = "tls_ms", serialize_with = "optional_millis")]
    pub tls: Option<Duration>,
    /// From the start of the call to the response headers.
    #[serde(rename = "first_byte_ms", serialize_with = "optional_millis")]
    pub first_byte: Option<Duration>,
    /// From the start of the call (connecting excluded) to its end.
    #[serde(rename = "total_ms", serialize_with = "millis")]
    pub total: Duration,
}

/// One gRPC call and its outcome.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GrpcExchange {
    /// Always `grpc`.
    pub protocol: &'static str,
    pub target: String,
    /// `package.Service/Method`.
    pub method: String,
    pub call_type: CallType,
    /// `grpc` or `grpc-web`.
    pub transport: &'static str,
    pub status: GrpcStatus,
    pub headers: Vec<MetadataEntry>,
    pub trailers: Vec<MetadataEntry>,
    /// Every received message, in order, as proto3 JSON.
    pub messages: Vec<Value>,
    /// One entry per sent and received message, for streaming calls.
    pub timeline: Vec<TimelineEntry>,
    pub timings: GrpcTimings,
    /// Set when a headless stream bound stopped the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<StreamStop>,
}

impl GrpcExchange {
    /// Whether the call type streams responses (`$.messages` in assertions).
    pub fn streams_responses(&self) -> bool {
        matches!(self.call_type, CallType::ServerStreaming | CallType::Bidi)
    }

    /// The single response message of a unary or client-streaming call.
    pub fn response(&self) -> Option<&Value> {
        if self.streams_responses() {
            return None;
        }
        self.messages.first()
    }

    /// The document JSONPath assertions read: the response message of a
    /// unary or client-streaming call (`{}` without one), else
    /// `{"messages": [...]}`.
    pub fn document(&self) -> Value {
        if self.streams_responses() {
            return json!({ "messages": self.messages });
        }
        self.response().cloned().unwrap_or_else(|| json!({}))
    }

    /// What assertions and captures read (design D7): the status and
    /// trailers, the response headers, [`GrpcExchange::document`] as the
    /// body, and the timings. There is no HTTP status (0).
    pub fn view(&self) -> ResponseView<'static> {
        ResponseView {
            status: 0,
            headers: pairs(&self.headers),
            body: Cow::Owned(self.document().to_string().into_bytes()),
            binary: false,
            ttfb: self.timings.first_byte.unwrap_or(self.timings.total),
            total: self.timings.total,
            grpc: Some(GrpcView {
                code: self.status.code,
                name: self.status.name.clone(),
                trailers: pairs(&self.trailers),
            }),
        }
    }

    /// Whether the call counts as a request error in `cli-runner` exit codes
    /// (exit 3): its deadline expired.
    pub fn is_request_error(&self) -> bool {
        self.status.code == DEADLINE_EXCEEDED
    }

    /// The credentials of the sensitive response metadata (`set-cookie`,
    /// `authorization`, ...), as an HTTP response's headers give them.
    pub fn credentials(&self) -> Redactor {
        let mut credentials = Redactor::new();
        for entry in self.headers.iter().chain(&self.trailers) {
            credentials.add_header_value(&entry.name, &entry.value);
        }
        credentials
    }

    /// The exchange with every secret `redactor` knows, and its own
    /// [`Self::credentials`], replaced: metadata values, messages, the
    /// status message and its details.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let mut redactor = redactor.clone();
        redactor.merge(&self.credentials());
        let redactor = &redactor;
        let text = |s: &mut String| *s = redactor.text(s).into_owned();
        for entry in self.headers.iter_mut().chain(self.trailers.iter_mut()) {
            text(&mut entry.value);
        }
        for message in &mut self.messages {
            *message = redact_value(message, redactor);
        }
        for entry in &mut self.timeline {
            entry.message = redact_value(&entry.message, redactor);
        }
        text(&mut self.status.message);
        for detail in &mut self.status.details {
            if let Some(value) = &mut detail.value {
                *value = redact_value(value, redactor);
            }
        }
        text(&mut self.target);
        self
    }
}

fn pairs(entries: &[MetadataEntry]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn millis<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(rustman_http::millis(*duration))
}

#[allow(clippy::ref_option)]
fn optional_millis<S: Serializer>(
    duration: &Option<Duration>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match duration {
        Some(d) => millis(d, serializer),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::Secret;

    use super::*;

    fn exchange(call_type: CallType) -> GrpcExchange {
        GrpcExchange {
            protocol: "grpc",
            target: "grpc://h:1".into(),
            method: "a.S/M".into(),
            call_type,
            transport: "grpc",
            status: GrpcStatus::new(0, ""),
            headers: vec![MetadataEntry::new("x-token", "tok-s3cr3t")],
            trailers: vec![MetadataEntry::new("grpc-status", "0")],
            messages: vec![json!({"id": "p-1", "note": "tok-s3cr3t"})],
            timeline: Vec::new(),
            timings: GrpcTimings::default(),
            stopped: None,
        }
    }

    #[test]
    fn documents_by_call_type() {
        assert_eq!(
            exchange(CallType::Unary).document(),
            json!({"id": "p-1", "note": "tok-s3cr3t"})
        );
        let stream = exchange(CallType::ServerStreaming);
        assert_eq!(stream.document()["messages"].as_array().unwrap().len(), 1);
        assert!(stream.response().is_none());
        let mut empty = exchange(CallType::ClientStreaming);
        empty.messages.clear();
        assert_eq!(empty.document(), json!({}));
    }

    #[test]
    fn the_view_carries_status_metadata_and_document() {
        let mut e = exchange(CallType::ServerStreaming);
        e.status = GrpcStatus::new(5, "missing");
        e.timings.total = Duration::from_millis(40);
        let view = e.view();
        let grpc = view.grpc.as_ref().unwrap();
        assert_eq!((grpc.code, grpc.label()), (5, "NOT_FOUND (5)".to_owned()));
        assert_eq!(view.metadata_values("grpc-status"), ["0"]);
        assert_eq!(view.header_values("x-token"), ["tok-s3cr3t"]);
        assert_eq!(view.json().unwrap()["messages"][0]["id"], "p-1");
        assert_eq!(view.ttfb, Duration::from_millis(40));
    }

    #[test]
    fn deadline_exceeded_is_a_request_error() {
        let mut e = exchange(CallType::Unary);
        assert!(!e.is_request_error());
        e.status = GrpcStatus::new(DEADLINE_EXCEEDED, "late");
        assert!(e.is_request_error());
    }

    #[test]
    fn redaction_canary() {
        let redactor = Redactor::new().with(&Secret::new("token", "tok-s3cr3t".to_owned()));
        let mut e = exchange(CallType::Bidi);
        e.timeline.push(TimelineEntry {
            direction: Direction::Sent,
            at: Duration::from_millis(1),
            size_bytes: 3,
            message: json!({"t": "tok-s3cr3t"}),
        });
        e.status.message = "denied for tok-s3cr3t".into();
        let shown = serde_json::to_string(&e.redacted(&redactor)).unwrap();
        assert!(!shown.contains("tok-s3cr3t"), "{shown}");
        assert!(shown.contains("«redacted:token»"), "{shown}");
        assert!(shown.contains("\"protocol\":\"grpc\""), "{shown}");
    }

    #[test]
    fn sensitive_response_metadata_is_redacted() {
        let mut e = exchange(CallType::Unary);
        e.headers.push(MetadataEntry::new(
            "set-cookie",
            "sid=cookie-c4n4ry-91; Path=/",
        ));
        e.trailers.push(MetadataEntry::new(
            "authorization",
            "Bearer bearer-c4n4ry-27",
        ));
        e.messages = vec![json!({"echo": "cookie-c4n4ry-91"})];
        let shown = serde_json::to_string(&e.redacted(&Redactor::new())).unwrap();
        assert!(!shown.contains("c4n4ry"), "{shown}");
        assert!(shown.contains("«redacted:Set-Cookie»"), "{shown}");
        assert!(shown.contains("«redacted:Authorization»"), "{shown}");
    }

    #[test]
    fn stop_messages() {
        assert_eq!(
            StreamStop::MaxMessages(100).message(),
            "stopped by rustman after 100 messages (stream.max_messages)"
        );
        assert_eq!(
            StreamStop::Duration(30_000).message(),
            "stopped by rustman after 30000 ms (stream.duration_ms)"
        );
    }
}
